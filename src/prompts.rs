//! What agents are told: the spec format and how to search, then each job's briefing with its
//! county or state, what neighbouring counties' adapters use, and what earlier attempts tried. A
//! jurisdictions pass is told its county's saved layers and what the last pass left to check.

use erisgis::Dataset;
use erisgis::counties::{Counties, County};
use erisgis::registry::Registry;
use erisgis::spec::{Source, Spec};

/// How to search, around the dataset's own guidance; `{many}` is the dataset's features.
const HOW: &str = r#"- Read pages by asking about them: `web read <url> which links lead to {many} or ArcGIS REST
  services?` answers in a few lines with the URLs. An open-data page links its service ("View API
  Resources", "I want to use this", a …/FeatureServer/0 URL); a web viewer's page or config names its
  services. Do not `web get` HTML pages: `web get` is for JSON endpoints, with --grep.
- `arcgis probe` a services directory, then a service, then its layer of {many}.
- Neighbouring counties often share a host or vendor (Schneider/Beacon, Tyler, Vision, a regional
  consortium, a state portal): try theirs first.
- Prefer the official layer: the county's or city's own, its state's, or a vendor's serving it for
  them. ArcGIS Online also holds copies people uploaded themselves (personal items, class projects,
  mirrors); use one only when nothing official is queryable, and submit it with "official": false
  and whose copy it is in "notes".
- Then `spec validate '<json>'`, read the report, fix what it names, and `spec submit` once it passes.

A county is unavailable only if, after a real search, its {many} are not published as a queryable
service or file. A viewer that draws {many} from a public ArcGIS service is available: use that
service. Never work around logins, keys, captchas, or terms that forbid automated access; those make a
county unavailable.

Be efficient: every command costs time, and `--help` on any command explains it. Do not repeat a
command you already ran."#;

pub fn system_prompt(dataset: &Dataset, state_job: bool) -> String {
    let goal = if state_job { dataset.agents.state_goal } else { dataset.agents.county_goal };
    format!(
        "{goal}\n\nHow to find {many}:\n{own}\n{how}",
        many = dataset.many,
        own = dataset.agents.how,
        how = HOW.replace("{many}", dataset.many)
    )
}

/// A jurisdictions pass's prompt: the dataset's goal for one, and how to search.
pub fn jurisdictions_prompt(dataset: &Dataset) -> String {
    format!(
        "{goal}\n\nHow to find {many}:\n{own}\n{how}",
        goal = dataset.agents.jurisdictions_goal.replace("{most}", &crate::work::LAYERS_PER_PASS.to_string()),
        many = dataset.many,
        own = dataset.agents.how,
        how = HOW.replace("{many}", dataset.many)
    )
}

fn source_line(spec: &Spec) -> String {
    let (kind, url) = match &spec.source {
        Source::ArcgisFeature { url, .. } => ("arcgis-feature", url),
        Source::Wfs { url, .. } => ("wfs", url),
        Source::OgcFeatures { url, .. } => ("ogc-features", url),
        Source::BulkDownload { url, .. } => ("bulk-download", url),
    };
    let filter = spec.filter.as_deref().map(|f| format!(" where {f}")).unwrap_or_default();
    format!("{} ({}): {kind} {url}{filter}", spec.id, spec.publisher)
}

/// Adapters verified for the county's neighbours, or claiming its state.
fn neighbour_adapters(registry: &Registry, counties: &Counties, county: &County) -> Vec<String> {
    let mut lines = Vec::new();
    for fips in counties.neighbours(&county.fips) {
        for id in registry.index.get(fips).into_iter().flatten() {
            if let (Some(spec), Some(n)) = (registry.adapters.get(id), counties.get(fips)) {
                lines.push(format!("- {}: {}", n.label(), source_line(spec)));
            }
        }
    }
    lines.sort();
    lines.dedup();
    lines.truncate(12);
    lines
}

pub fn county_briefing(
    dataset: &Dataset,
    county: &County,
    counties: &Counties,
    registry: &Registry,
    summaries: &[String],
    dead: Option<(&Spec, &str)>,
) -> String {
    let b = county.bbox;
    let mut text = format!(
        "Your county: {} ({}), FIPS {}.\nBounding box (lon/lat): west {:.4}, south {:.4}, east {:.4}, north {:.4}. \
         A point inside it: {:.5},{:.5}. Validation samples {} around that point.",
        county.name,
        county.state_name,
        county.fips,
        b.west(),
        b.south(),
        b.east(),
        b.north(),
        county.interior[0],
        county.interior[1],
        dataset.many,
    );
    let neighbours = neighbour_adapters(registry, counties, county);
    if !neighbours.is_empty() {
        text.push_str("\n\nNeighbouring counties' adapters (try the same hosts and patterns first):\n");
        text.push_str(&neighbours.join("\n"));
    }
    let claims = registry.unverified_claims(&county.fips, &county.state);
    if !claims.is_empty() {
        text.push_str("\n\nThese adapters claim your county but do not pass for it:\n");
        for spec in claims.iter().take(5) {
            text.push_str(&format!("- {}\n", source_line(spec)));
        }
    }
    if let Some((spec, report)) = dead {
        text.push_str(&format!(
            "\n\nYour county's adapter stopped working. Its spec was:\n{}\nIts validation now fails: {report}\n\
             Find where the service moved (or what replaced it) and submit a fixed spec; keep its id if the \
             source is the same layer, otherwise choose a new id.",
            serde_json::to_string_pretty(spec).unwrap_or_default()
        ));
    }
    if !summaries.is_empty() {
        text.push_str("\n\nEarlier attempts at this county, which did not succeed, reported:\n");
        for (i, summary) in summaries.iter().enumerate() {
            text.push_str(&format!("--- attempt {} ---\n{}\n", i + 1, summary.trim()));
        }
        text.push_str("Start from what they learned; do not repeat what failed.");
    }
    text
}

/// A covered county's briefing for a jurisdictions pass: its saved layers, what each holds, and
/// what the last pass said it left.
pub fn jurisdictions_briefing(
    dataset: &Dataset,
    county: &County,
    counties: &Counties,
    registry: &Registry,
    last_pass: Option<&str>,
    summaries: &[String],
) -> String {
    let b = county.bbox;
    let mut text = format!(
        "Your county: {} ({}), FIPS {}.\nBounding box (lon/lat): west {:.4}, south {:.4}, east {:.4}, north {:.4}.\n\n\
         Its saved layers of {}:",
        county.name,
        county.state_name,
        county.fips,
        b.west(),
        b.south(),
        b.east(),
        b.north(),
        dataset.many,
    );
    for spec in registry.index.get(&county.fips).into_iter().flatten().filter_map(|id| registry.adapters.get(id)) {
        let holds = if spec.fields.contains_key("jurisdiction") {
            format!(" Holds several jurisdictions, named in its {} attribute.", spec.fields["jurisdiction"])
        } else {
            String::new()
        };
        let notes = if spec.notes.is_empty() { String::new() } else { format!(" Notes: {}", spec.notes) };
        text.push_str(&format!("\n- {}.{holds}{notes}", source_line(spec)));
    }
    let neighbours = neighbour_adapters(registry, counties, county);
    if !neighbours.is_empty() {
        text.push_str("\n\nNeighbouring counties' adapters (their hosts may serve your jurisdictions too):\n");
        text.push_str(&neighbours.join("\n"));
    }
    if let Some(report) = last_pass {
        text.push_str(&format!(
            "\n\nThe last pass reported:\n{}\nGo on from there: do not check again what it found unpublished.",
            report.trim()
        ));
    }
    if !summaries.is_empty() {
        text.push_str("\n\nEarlier attempts at this pass, which did not finish, reported:\n");
        for (i, summary) in summaries.iter().enumerate() {
            text.push_str(&format!("--- attempt {} ---\n{}\n", i + 1, summary.trim()));
        }
    }
    text
}

pub fn state_briefing(
    dataset: &Dataset,
    state: &str,
    state_name: &str,
    counties: usize,
    summaries: &[String],
) -> String {
    let mut text = format!(
        "Your state: {state_name} ({state}), {counties} counties. Find a statewide layer of {}, if one exists.",
        dataset.many
    );
    if !summaries.is_empty() {
        text.push_str("\n\nEarlier attempts reported:\n");
        for summary in summaries {
            text.push_str(&format!("- {}\n", summary.trim()));
        }
    }
    text
}

pub const NUDGE: &str = "You stopped without `spec submit` or `spec unavailable`. Keep going: validate \
    and submit a spec, or record the county unavailable with evidence. If you are truly stuck, reply with \
    a short summary of what you tried (URLs, layers, what failed) instead.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_are_told_to_prefer_official_layers_and_mark_copies() {
        for dataset in erisgis::dataset::ALL {
            for statewide in [false, true] {
                let prompt = system_prompt(dataset, statewide);
                assert!(prompt.contains("Prefer the official layer"), "{prompt}");
                assert!(prompt.contains("\"official\": false"), "{prompt}");
                assert!(prompt.contains(&format!("`arcgis search {}`", dataset.agents.search)), "{prompt}");
                assert!(!prompt.contains("{many}"), "{prompt}");
            }
        }
    }

    #[test]
    fn zoning_agents_look_for_every_jurisdictions_districts_and_not_plans() {
        let prompt = system_prompt(&erisgis::zoning::ZONING, false);
        for words in ["largest cities", "not future land use", "none-exists", "zoning districts are not published"] {
            assert!(prompt.contains(words), "{words}: {prompt}");
        }
    }
}
