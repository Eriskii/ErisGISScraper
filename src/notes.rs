//! Field notes from a run, as Markdown: where the registry stands, the counties recorded
//! unavailable by reason with their evidence, the counties agents could not finish grouped by
//! what stopped them, the hosts behind the most trouble, the jurisdictions passes over covered
//! counties and what each last said, and what the run cost.

use crate::journal::{Event, Job, Outcome, Tokens};
use erisgis::counties::Counties;
use erisgis::registry::{Registry, Standing};
use erisgis::spec::Coverage;
use regex::Regex;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

/// What stopped an agent, recognized in its own summary, in the order the groups are tried.
const CAUSES: [(&str, &str); 7] = [
    ("Expired or broken HTTPS certificates", r"certificat|\bssl\b|\btls\b|error sending request"),
    (
        "Public services that refuse access",
        r"do not have permission|not authori[sz]ed|token required|\b(401|403|499)\b|login",
    ),
    (
        "Bot checks in front of public records",
        r"captcha|bot[- ]check|bot check|cloudflare|interstitial|are you a robot",
    ),
    ("Servers too slow to answer", r"timed? out|\b50[234]\b|unable to perform query|too slowly|took longer"),
    ("Samples that found nothing", r"\bsample\b|interior point|at least \d|minimum of \d"),
    ("Paging that does not work", r"paging|resultoffset|offset"),
    ("Fields that do not map", r"field|attribute"),
];

/// A county's last word in the journal.
struct Last<'a> {
    rung: usize,
    outcome: &'a Outcome,
}

fn host(url: &str) -> Option<String> {
    url::Url::parse(url.trim_end_matches(['.', ',', ')', '`', '*'])).ok()?.host_str().map(str::to_owned)
}

fn shorten(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max { flat } else { format!("{}…", flat.chars().take(max).collect::<String>()) }
}

fn tokens_line(tokens: &Tokens) -> String {
    format!("{} in ({} cached), {} out", tokens.input, tokens.cached_input, tokens.output)
}

/// The notes for `registry` and the run its `events` tell, at `now` (Unix milliseconds).
pub fn notes(counties: &Counties, registry: &Registry, events: &[Event], now: u64) -> String {
    let mut last: HashMap<&str, Last> = HashMap::new();
    // Each county's jurisdictions passes: how many ended, and the last one's outcome.
    let mut passes: BTreeMap<&str, (usize, &Outcome)> = BTreeMap::new();
    let mut total = Tokens::default();
    let mut by_model: BTreeMap<String, (usize, Tokens)> = BTreeMap::new();
    let mut tools: BTreeMap<&str, Tokens> = BTreeMap::new();
    let mut models: HashMap<&str, &str> = HashMap::new();
    for event in events {
        match event {
            Event::Started { agent, model, .. } => {
                models.insert(agent, model);
            }
            Event::Finished { job, rung, agent, outcome, tokens, tools: used, .. } => {
                total.add(tokens);
                let model = models.get(agent.as_str()).copied().unwrap_or("unknown");
                let entry = by_model.entry(model.to_owned()).or_default();
                entry.0 += 1;
                entry.1.add(tokens);
                for (name, spent) in used {
                    tools.entry(name).or_default().add(spent);
                }
                match job {
                    Job::County(fips) => {
                        last.insert(fips, Last { rung: *rung, outcome });
                    }
                    Job::Jurisdictions(fips) => {
                        let entry = passes.entry(fips).or_insert((0, outcome));
                        *entry = (entry.0 + 1, outcome);
                    }
                    Job::State(_) => {}
                }
            }
            Event::Checked { .. } => {}
        }
    }

    let (mut verified, mut claimed, mut unavailable, mut stuck, mut untried) = (0, 0, 0, Vec::new(), 0);
    for county in &counties.list {
        match registry.standing(&county.fips, now) {
            Standing::Covered(_) => verified += 1,
            Standing::Unavailable(_) => unavailable += 1,
            Standing::Open if !registry.unverified_claims(&county.fips, &county.state).is_empty() => claimed += 1,
            Standing::Open => match last.get(county.fips.as_str()) {
                Some(l) if matches!(l.outcome, Outcome::GaveUp { .. } | Outcome::Failed { .. }) => {
                    stuck.push((county, l))
                }
                _ => untried += 1,
            },
        }
    }
    let statewide = registry.adapters.values().filter(|s| matches!(s.coverage, Coverage::States(_))).count();

    let mut out = String::new();
    let _ = writeln!(out, "# Field notes\n");
    let _ = writeln!(out, "## Where it stands\n");
    let _ = writeln!(out, "| | Counties |\n|---|---:|");
    let _ = writeln!(out, "| Covered, verified for the county | {verified} |");
    let _ = writeln!(out, "| Covered by a statewide layer, not yet verified there | {claimed} |");
    let _ = writeln!(out, "| Recorded unavailable | {unavailable} |");
    let _ = writeln!(out, "| Agents could not finish | {} |", stuck.len());
    let _ = writeln!(out, "| Not yet tried | {untried} |");
    let _ = writeln!(out, "| **All** | **{}** |\n", counties.list.len());
    let _ = writeln!(out, "{} adapters, {statewide} of them statewide.\n", registry.adapters.len());

    let copies: Vec<_> = registry.adapters.values().filter(|s| s.official == Some(false)).collect();
    let unsaid = registry.adapters.values().filter(|s| s.official.is_none()).count();
    let _ = writeln!(out, "## Unofficial copies ({})\n", copies.len());
    for spec in &copies {
        let covers = match &spec.coverage {
            Coverage::Counties(list) => {
                list.iter().map(|f| counties.get(f).map_or(f.clone(), |c| c.label())).collect::<Vec<_>>().join(", ")
            }
            Coverage::States(states) => format!("statewide: {}", states.join(", ")),
        };
        let _ = writeln!(out, "- **{}** ({covers}): {}. {}", spec.id, spec.publisher, shorten(&spec.notes, 300));
    }
    let _ = writeln!(
        out,
        "\n{unsaid} adapter{} not say whether {} official.\n",
        if unsaid == 1 { " does" } else { "s do" },
        if unsaid == 1 { "it is" } else { "they are" }
    );

    let _ = writeln!(out, "## Unavailable, by reason\n");
    let mut reasons: BTreeMap<&str, Vec<_>> = BTreeMap::new();
    for record in registry.unavailable.values() {
        reasons.entry(record.reason.name()).or_default().push(record);
    }
    for (reason, records) in &reasons {
        let _ = writeln!(out, "### {reason} ({})\n", records.len());
        for record in records {
            let name = counties.get(&record.fips).map_or(record.fips.clone(), |c| c.label());
            let evidence: Vec<String> = record.evidence.iter().map(|e| format!("<{e}>")).collect();
            let _ = writeln!(out, "- **{name}**: {} {}", shorten(&record.note, 300), evidence.join(" "));
        }
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "## What stopped the agents\n");
    let patterns: Vec<(&str, Regex)> =
        CAUSES.iter().map(|(name, p)| (*name, Regex::new(&format!("(?i){p}")).expect("a pattern"))).collect();
    let url = Regex::new(r"https?://[^\s)`'\]>]+").expect("a pattern");
    let mut groups: BTreeMap<usize, Vec<(String, usize, String)>> = BTreeMap::new();
    let mut hosts: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (county, l) in &stuck {
        let text = match l.outcome {
            Outcome::GaveUp { summary } => summary.as_str(),
            Outcome::Failed { error } => error.as_str(),
            _ => "",
        };
        let group = patterns.iter().position(|(_, p)| p.is_match(text)).unwrap_or(patterns.len());
        groups.entry(group).or_default().push((county.label(), l.rung, shorten(text, 400)));
        let mut seen: Vec<String> = url.find_iter(text).filter_map(|m| host(m.as_str())).collect();
        seen.sort();
        seen.dedup();
        for h in seen {
            hosts.entry(h).or_default().push(county.label());
        }
    }
    for (group, entries) in &groups {
        let name = patterns.get(*group).map_or("Everything else", |(name, _)| name);
        let _ = writeln!(out, "### {name} ({})\n", entries.len());
        for (label, rung, text) in entries {
            let _ = writeln!(out, "- **{label}** (stopped on rung {rung}): {text}");
        }
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "## Hosts behind the most trouble\n");
    let mut ranked: Vec<_> = hosts.into_iter().filter(|(_, c)| c.len() > 1).collect();
    ranked.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));
    if ranked.is_empty() {
        let _ = writeln!(out, "No host stands behind more than one stuck county.");
    }
    for (host, names) in ranked.iter().take(20) {
        let _ = writeln!(out, "- `{host}`: {} counties ({})", names.len(), names.join(", "));
    }

    if !passes.is_empty() {
        let _ = writeln!(
            out,
            "\n## Jurisdictions passes ({} count{})\n",
            passes.len(),
            if passes.len() == 1 { "y" } else { "ies" }
        );
        for (fips, (count, outcome)) in &passes {
            let name = counties.get(fips).map_or(fips.to_string(), |c| c.label());
            let said = match outcome {
                Outcome::Done { detail, .. } => shorten(detail, 600),
                Outcome::Failed { error } => format!("failed: {}", shorten(error, 300)),
                Outcome::GaveUp { summary } => shorten(summary, 600),
                Outcome::Unavailable { reason } => reason.clone(),
            };
            let _ = writeln!(out, "- **{name}** ({count} pass{}): {said}", if *count == 1 { "" } else { "es" });
        }
    }

    let _ = writeln!(out, "\n## What it cost\n");
    let _ = writeln!(out, "All attempts: {}.\n", tokens_line(&total));
    let _ = writeln!(out, "| Model | Attempts | Tokens |\n|---|---:|---|");
    for (model, (attempts, tokens)) in &by_model {
        let _ = writeln!(out, "| {model} | {attempts} | {} |", tokens_line(tokens));
    }
    if !tools.is_empty() {
        let _ = writeln!(out, "\nTools' own model calls, counted above too:\n");
        for (name, tokens) in &tools {
            let _ = writeln!(out, "- `{name}`: {}", tokens_line(tokens));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Tokens;
    use erisgis::spec::{Reason, Unavailable};
    use serde_json::json;

    fn finished(fips: &str, rung: usize, agent: &str, outcome: Outcome) -> Event {
        Event::Finished {
            at: 1,
            job: Job::County(fips.into()),
            rung,
            agent: agent.into(),
            outcome,
            tokens: Tokens { input: 100, cached_input: 50, output: 10 },
            tools: BTreeMap::from([("web read".into(), Tokens { input: 20, cached_input: 0, output: 2 })]),
        }
    }

    fn started(fips: &str, agent: &str, model: &str) -> Event {
        Event::Started { at: 0, job: Job::County(fips.into()), rung: 0, model: model.into(), agent: agent.into() }
    }

    #[test]
    fn notes_group_what_stopped_agents_and_name_the_hosts_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let counties = Counties::embedded().unwrap();
        let mut registry = Registry::open(dir.path()).unwrap();
        registry
            .mark_unavailable(Unavailable {
                version: 1,
                fips: "13195".into(),
                reason: Reason::TosForbidsAutomation,
                evidence: vec!["https://qpublic.example/terms".into()],
                note: "A bot check stands in front of the records.".into(),
                recorded_at: 0,
                recheck_after: u64::MAX,
                model: None,
            })
            .unwrap();
        let cert = |county: &str| Outcome::GaveUp {
            summary: format!(
                "Every request to https://ags.bhamaps.com/arcgisserver/rest/services/{county}/MapServer/0 fails with \"error sending request\"."
            ),
        };
        let pass = |detail: &str, more: bool| Event::Finished {
            at: 2,
            job: Job::Jurisdictions("26163".into()),
            rung: 0,
            agent: "d".into(),
            outcome: Outcome::Done { detail: detail.into(), more },
            tokens: Tokens::default(),
            tools: BTreeMap::new(),
        };
        let events = [
            pass("Saved mi-wayne-dearborn. Livonia is left.", true),
            pass("Saved mi-wayne-livonia. Hamtramck publishes its zoning only as a PDF.", false),
            started("17117", "a", "claude-haiku-5-5"),
            finished("17117", 2, "a", cert("MacoupinIL")),
            started("17101", "b", "claude-sonnet-5-5"),
            finished("17101", 1, "b", cert("LawrenceIL")),
            started("16073", "c", "claude-haiku-5-5"),
            finished(
                "16073",
                0,
                "c",
                Outcome::GaveUp { summary: "The server says \"You do not have permissions\".".into() },
            ),
        ];
        let mut copy = |id: &str, official: Option<bool>| {
            let mut value = json!({
                "version": 1, "id": id, "publisher": "Someone's ArcGIS Online",
                "source": {"kind": "arcgis-feature", "url": "https://services.arcgis.com/x/FeatureServer/0"},
                "coverage": {"counties": ["26163"]}, "fields": {"apn": "PIN"},
                "notes": "A personal hosted copy; Wayne County publishes no queryable layer.",
            });
            if let Some(official) = official {
                value["official"] = json!(official);
            }
            registry.save_adapter(serde_json::from_value(value).unwrap()).unwrap();
        };
        copy("mi-wayne-copy", Some(false));
        copy("mi-unsaid", None);
        copy("mi-official", Some(true));
        let text = notes(&counties, &registry, &events, 0);
        let section = |title: &str| {
            text.split(title)
                .nth(1)
                .unwrap_or_else(|| panic!("no {title} in {text}"))
                .split("\n#")
                .next()
                .unwrap()
                .to_owned()
        };
        let copies = section("## Unofficial copies (1)");
        assert!(copies.contains("mi-wayne-copy") && copies.contains("Wayne County, MI"), "{text}");
        assert!(copies.contains("1 adapter does not say whether it is official."), "{text}");
        let certs = section("### Expired or broken HTTPS certificates (2)");
        assert!(certs.contains("Macoupin County, IL") && certs.contains("Lawrence County, IL"), "{text}");
        assert!(section("### Public services that refuse access (1)").contains("Owyhee County, ID"));
        assert!(text.contains("`ags.bhamaps.com`: 2 counties"), "{text}");
        assert!(text.contains("### tos-forbids-automation (1)") && text.contains("<https://qpublic.example/terms>"));
        assert!(text.contains("| Agents could not finish | 3 |"));
        assert!(text.contains("| claude-haiku-5-5 | 2 | 200 in (100 cached), 20 out |"), "{text}");
        assert!(text.contains("`web read`: 60 in (0 cached), 6 out"));
        assert!(
            section("## Jurisdictions passes (1 county)")
                .contains("**Wayne County, MI (26163)** (2 passes): Saved mi-wayne-livonia. Hamtramck publishes"),
            "{text}"
        );
    }
}
