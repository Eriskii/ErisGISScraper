//! Whole runs against a stand-in model and a fake county server: agents validate and submit
//! specs, give up and hand over to the next rung, cover a state with one layer, and resume
//! after the run is stopped mid-turn.

mod support;

use erisagentauth::{Accounts, App};
use erisgis::counties::Counties;
use erisgis::http::{Http, Limits};
use erisgis::registry::Registry;
use erisgis::runtime::{Runtime, VALIDATE_WITHIN};
use erisgisscraper::journal::{Event, Job, Journal, Outcome};
use erisgisscraper::models::{self, CODEX, Providers, Rung};
use erisgisscraper::scheduler::{Scheduler, Settings};
use erisgisscraper::tools;
use erisgisscraper::work::{LAYERS_PER_PASS, PASSES_IN_A_ROW, Work};
use erisharness::Harness;
use erisharness::provider::{Provider, RateGate, Responses, ResponsesConfig, StaticToken};
use erisharness::tools::web::Models;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::model::{Brain, Model, Reply, Request};

fn ladder() -> Vec<Rung> {
    vec![
        Rung { provider: CODEX.into(), model: "mini".into(), effort: None, context_window: None },
        Rung { provider: CODEX.into(), model: "big".into(), effort: Some("high".into()), context_window: None },
    ]
}

async fn scheduler(dir: &Path, model: &Model, counties: Vec<String>, statewide: bool) -> Arc<Scheduler> {
    scheduler_for(dir, model, counties, statewide, &erisgis::parcels::PARCELS).await
}

async fn scheduler_for(
    dir: &Path,
    model: &Model,
    counties: Vec<String>,
    statewide: bool,
    dataset: &'static erisgis::Dataset,
) -> Arc<Scheduler> {
    let provider: Arc<dyn Provider> = Arc::new(Responses::new(ResponsesConfig {
        base_url: model.url.clone(),
        headers: vec![],
        store: false,
        max_retries: 0,
        credentials: Arc::new(StaticToken("token".into())),
        gate: RateGate::new(16, 64),
    }));
    scheduler_on(dir, vec![(CODEX, provider)], Models::default(), ladder(), counties, statewide, dataset).await
}

async fn scheduler_on(
    dir: &Path,
    providers: Vec<(&'static str, Arc<dyn Provider>)>,
    web: Models,
    ladder: Vec<Rung>,
    counties: Vec<String>,
    statewide: bool,
    dataset: &'static erisgis::Dataset,
) -> Arc<Scheduler> {
    let all = Arc::new(Counties::embedded().unwrap());
    let work = Arc::new(Work {
        dataset,
        counties: all,
        registry: Mutex::new(Registry::open(&dir.join("registry")).unwrap()),
        runtime: Runtime {
            http: Http::new(Limits { allow_private: true, ..Limits::default() }).unwrap(),
            cache: dir.join("cache"),
            validate_within: VALIDATE_WITHIN,
        },
        web,
        web_http: erisharness::tools::web::Http::new(
            erisharness::tools::web::Limits { allow_private: true, ..Default::default() },
            "erisgisscraper-test",
        )
        .unwrap(),
        agents: Mutex::default(),
        validate_budget: 3,
        log: Mutex::default(),
        logged: Default::default(),
    });
    let empty = dir.join("harness/empty");
    std::fs::create_dir_all(&empty).unwrap();
    let mut builder = Harness::builder(dir.join("harness")).tools(tools::all(&work));
    for (name, provider) in providers {
        builder = builder.provider(name, provider);
    }
    let harness = builder.open().await.unwrap();
    let (journal, past) = Journal::open(&dir.join("journal.jsonl")).unwrap();
    let settings = Settings {
        concurrency: 4,
        ladder,
        attempts_per_rung: 1,
        statewide,
        retry_failed: false,
        jurisdictions: false,
        again: false,
    };
    Scheduler::new(work, harness, journal, past, settings, counties, empty, Box::new(|| Box::pin(async { true })))
}

fn good_spec(root: &str, coverage: Value) -> Value {
    json!({
        "id": "fake-parcels", "publisher": "Fake GIS", "official": true,
        "source": {"kind": "arcgis-feature", "url": format!("{root}/arcgis/good/FeatureServer/0")},
        "coverage": coverage,
        "fields": {"apn": "PARCELNO", "owner": "OWNER", "address": "SITEADDR", "acres": "GISACRES"},
        "filter": "STATUS = 'ACTIVE'",
    })
}

/// A word in single quotes, as a shell would take it.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Validates the spec with `program`, submits it once it passes, then says it is done.
fn diligent(request: &Request, program: &str, spec: &Value) -> Reply {
    let last = request.last_output();
    let command = |line: String| Reply::Calls(program.into(), json!({"command": line}));
    if last.contains("Saved ") {
        Reply::Says("Saved the adapter.".into())
    } else if last.contains("\"passed\": true") {
        command("submit".into())
    } else {
        command(format!("validate {}", quoted(&spec.to_string())))
    }
}

fn events(dir: &Path) -> Vec<Event> {
    Journal::open(&dir.join("journal.jsonl")).unwrap().1
}

fn interiors(fips: &[&str]) -> Vec<(f64, f64)> {
    let all = Counties::embedded().unwrap();
    fips.iter().map(|f| all.get(f).unwrap()).map(|c| (c.interior[0], c.interior[1])).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_county_that_the_small_model_gives_up_on_is_covered_on_the_next_rung() {
    let dir = tempfile::tempdir().unwrap();
    let root = support::serve(support::Grid::around_all(&interiors(&["55025"]))).await;
    let spec = good_spec(&root, json!({"counties": ["55025"]}));
    let brain: Brain = Arc::new(move |request: &Request| match request.model.as_str() {
        "mini" => Reply::Says("I searched the Dane County site and found only a viewer.".into()),
        _ => diligent(request, "spec", &spec),
    });
    let model = Model::start(brain).await;
    let run = scheduler(dir.path(), &model, vec!["55025".into()], false).await;
    tokio::time::timeout(Duration::from_secs(60), run.run()).await.unwrap().unwrap();

    let registry = Registry::open(&dir.path().join("registry")).unwrap();
    let saved = &registry.adapters["fake-parcels"];
    assert_eq!(saved.verification.rung, Some(1));
    assert_eq!(saved.verification.model.as_deref(), Some("big"));
    assert_eq!(saved.filter.as_deref(), Some("STATUS = 'ACTIVE'"), "single quotes survive the command line");
    assert_eq!(registry.index["55025"], ["fake-parcels"]);

    let outcomes: Vec<(usize, &str)> = events(dir.path())
        .iter()
        .filter_map(|e| match e {
            Event::Finished { rung, outcome, .. } => Some((
                *rung,
                match outcome {
                    Outcome::Done { .. } => "done",
                    Outcome::GaveUp { .. } => "gave-up",
                    _ => "other",
                },
            )),
            _ => None,
        })
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    assert_eq!(outcomes, [(0, "gave-up"), (1, "done")]);

    let requests = model.requests.lock().unwrap();
    let mini: Vec<&Request> = requests.iter().filter(|r| r.model == "mini").collect();
    assert_eq!(mini.len(), 2, "the small model is nudged once, then gives up");
    assert!(mini[1].user.last().unwrap().contains("stopped without `spec submit`"));
    let big = requests.iter().find(|r| r.model == "big").unwrap();
    assert!(big.briefing().contains("Dane County"));
    assert!(big.briefing().contains("found only a viewer"), "the next rung reads what the last one tried");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_statewide_layer_covers_the_state_without_county_agents() {
    let dir = tempfile::tempdir().unwrap();
    let selected = ["55001", "55025", "55071", "55141"];
    let root = support::serve(support::Grid::around_all(&interiors(&selected))).await;
    let spec = good_spec(&root, json!({"states": ["WI"]}));
    let brain: Brain = Arc::new(move |request: &Request| {
        assert!(
            request.briefing().contains("Your state: Wisconsin"),
            "only the state's agent runs: {}",
            request.briefing()
        );
        diligent(request, "statewide_spec", &spec)
    });
    let model = Model::start(brain).await;
    let run = scheduler(dir.path(), &model, selected.map(String::from).to_vec(), true).await;
    tokio::time::timeout(Duration::from_secs(60), run.run()).await.unwrap().unwrap();

    let registry = Registry::open(&dir.path().join("registry")).unwrap();
    for fips in selected {
        assert_eq!(registry.index.get(fips).map(Vec::as_slice), Some(&["fake-parcels".to_owned()][..]), "{fips}");
    }
    assert_eq!(registry.adapters["fake-parcels"].verification.counties, selected);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_county_without_a_service_is_marked_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let brain: Brain = Arc::new(|request: &Request| {
        if request.outputs.is_empty() {
            Reply::Calls(
                "spec".into(),
                json!({"command": "unavailable paywalled --evidence https://county.example/store --note \"Parcels are sold\""}),
            )
        } else {
            Reply::Says("Marked unavailable.".into())
        }
    });
    let model = Model::start(brain).await;
    let run = scheduler(dir.path(), &model, vec!["13121".into()], false).await;
    tokio::time::timeout(Duration::from_secs(60), run.run()).await.unwrap().unwrap();
    let registry = Registry::open(&dir.path().join("registry")).unwrap();
    let record = &registry.unavailable["13121"];
    assert_eq!(record.reason.name(), "paywalled");
    assert_eq!(record.evidence, ["https://county.example/store"]);
    assert_eq!(record.note, "Parcels are sold");
    assert!(record.recheck_after > record.recorded_at + 300 * 86_400_000);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_run_resumes_its_agent_where_it_stood() {
    let dir = tempfile::tempdir().unwrap();
    let root = support::serve(support::Grid::around_all(&interiors(&["55025"]))).await;

    // First run: the model never answers; the run is stopped mid-turn.
    let hanging = Model::start(Arc::new(|_: &Request| Reply::Hangs)).await;
    let run = scheduler(dir.path(), &hanging, vec!["55025".into()], false).await;
    let task = tokio::spawn({
        let run = run.clone();
        async move { run.run().await }
    });
    tokio::time::timeout(Duration::from_secs(20), async {
        while hanging.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    run.stop().await;
    task.await.unwrap().unwrap();
    drop(run);
    let started: Vec<String> = events(dir.path())
        .iter()
        .filter_map(|e| if let Event::Started { agent, .. } = e { Some(agent.clone()) } else { None })
        .collect();
    assert_eq!(started.len(), 1);

    // Second run: the same agent goes on and finishes; no new attempt starts.
    let spec = good_spec(&root, json!({"counties": ["55025"]}));
    let working = Model::start(Arc::new(move |request: &Request| diligent(request, "spec", &spec))).await;
    let run = scheduler(dir.path(), &working, vec!["55025".into()], false).await;
    tokio::time::timeout(Duration::from_secs(60), run.run()).await.unwrap().unwrap();
    let all = events(dir.path());
    let agents: Vec<(&str, &str)> = all
        .iter()
        .filter_map(|e| match e {
            Event::Started { agent, .. } => Some(("started", agent.as_str())),
            Event::Finished { agent, .. } => Some(("finished", agent.as_str())),
            Event::Checked { .. } => None,
        })
        .collect();
    assert_eq!(agents, [("started", started[0].as_str()), ("finished", started[0].as_str())]);
    assert!(Registry::open(&dir.path().join("registry")).unwrap().index.contains_key("55025"));
}

/// Claude Code's credentials file, holding `access` for the next hour.
fn claude_code_file(path: &Path, access: &str) {
    let expires =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 + 3_600_000;
    let text = json!({"claudeAiOauth": {"accessToken": access, "refreshToken": "claude-code-refresh",
        "expiresAt": expires, "scopes": ["user:inference"], "subscriptionType": "max"}});
    std::fs::write(path, text.to_string()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn claude_rungs_run_on_the_bound_claude_code_login() {
    let dir = tempfile::tempdir().unwrap();
    let root = support::serve(support::Grid::around_all(&interiors(&["55025"]))).await;
    let spec = good_spec(&root, json!({"counties": ["55025"]}));
    let brain: Brain = Arc::new(move |request: &Request| {
        if request.web_search {
            return Reply::Says("https://dane.example.gov/gis — Dane County's GIS page".into());
        }
        match request.model.as_str() {
            "claude-haiku-9" if request.outputs.is_empty() => {
                Reply::Calls("web".into(), json!({"command": "search dane county parcels"}))
            }
            "claude-haiku-9" => Reply::Says("Dane County shows parcels only in a viewer.".into()),
            _ => diligent(request, "spec", &spec),
        }
    });
    let model = Model::start(brain).await;

    let credentials = dir.path().join("claude-code.json");
    claude_code_file(&credentials, "from-claude-code");
    let app = App { name: "test", originator: "test" };
    let accounts = Accounts::load_from(&dir.path().join("accounts"), app, &model.url, &model.url, &model.url).unwrap();
    accounts.claude.bind(&credentials).await.unwrap();

    let listed = accounts.claude.models().await.unwrap();
    let ladder = models::parse_ladder("haiku:low, sonnet:high", &listed).unwrap();
    let providers = Providers::new(&accounts.openai, &accounts.claude, &model.url).await.unwrap();
    let web = providers.web(&listed);
    let run = scheduler_on(
        dir.path(),
        providers.named(),
        web,
        ladder,
        vec!["55025".into()],
        false,
        &erisgis::parcels::PARCELS,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(60), run.run()).await.unwrap().unwrap();

    let registry = Registry::open(&dir.path().join("registry")).unwrap();
    let saved = &registry.adapters["fake-parcels"];
    assert_eq!(saved.verification.model.as_deref(), Some("claude-sonnet-9"));
    assert_eq!(saved.verification.rung, Some(1));
    let requests = model.requests.lock().unwrap();
    assert!(requests.iter().any(|r| r.model == "claude-haiku-9"), "haiku resolves to the newest Haiku");
    let searched = self::events(dir.path())
        .into_iter()
        .find_map(|e| match e {
            Event::Finished { rung: 0, tokens, tools, .. } => Some((tokens, tools)),
            _ => None,
        })
        .unwrap();
    assert_eq!(searched.1["web search"].input, 300, "the journal names what the search spent");
    assert!(searched.0.input > 300, "and counts it in the attempt's tokens");
    for request in requests.iter().filter(|r| !r.web_search) {
        let effort = if request.model == "claude-haiku-9" { "low" } else { "high" };
        assert_eq!(request.thinking.as_deref(), Some("adaptive"));
        assert_eq!(request.effort.as_deref(), Some(effort), "each rung's effort reaches Claude");
        assert_eq!(request.provider, "anthropic");
        assert_eq!(request.authorization, "Bearer from-claude-code", "every call carries the bound file's token");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_run_stops_a_validation_under_way() {
    let dir = tempfile::tempdir().unwrap();
    let root = support::serve(support::Grid::around_all(&interiors(&["55025"]))).await;
    let stalled = json!({
        "id": "stalled", "publisher": "Slow GIS",
        "source": {"kind": "arcgis-feature", "url": format!("{root}/arcgis/stalls/FeatureServer/0")},
        "coverage": {"counties": ["55025"]}, "fields": {"apn": "PARCELNO"},
    });
    let brain: Brain = Arc::new(move |request: &Request| {
        if request.outputs.is_empty() {
            Reply::Calls("spec".into(), json!({"command": format!("validate {}", quoted(&stalled.to_string()))}))
        } else {
            Reply::Hangs
        }
    });
    let model = Model::start(brain).await;
    let run = scheduler(dir.path(), &model, vec!["55025".into()], false).await;
    let runner = {
        let run = run.clone();
        tokio::spawn(async move { run.run().await })
    };
    // The agent is inside its validate call, waiting on the stalled server.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(10), run.stop()).await.expect("stopping waits for no validation");
    assert!(started.elapsed() < Duration::from_secs(5));
    runner.abort();
}

/// A statewide adapter for Alabama read from `url`, saved unverified, as a state's search
/// leaves it.
fn claim_alabama(dir: &Path, url: String) {
    let spec: erisgis::spec::Spec = serde_json::from_value(json!({
        "version": 1, "id": "al-statewide", "publisher": "Alabama GIS",
        "source": {"kind": "arcgis-feature", "url": url},
        "coverage": {"states": ["AL"]}, "fields": {"apn": "PARCELNO"},
    }))
    .unwrap();
    Registry::open(&dir.join("registry")).unwrap().save_adapter(spec).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_slow_statewide_check_does_not_hold_back_county_agents() {
    let dir = tempfile::tempdir().unwrap();
    let root = support::serve(support::Grid::around_all(&interiors(&["13121"]))).await;
    claim_alabama(dir.path(), format!("{root}/arcgis/stalls/FeatureServer/0"));
    let spec = good_spec(&root, json!({"counties": ["13121"]}));
    let brain: Brain = Arc::new(move |request: &Request| diligent(request, "spec", &spec));
    let model = Model::start(brain).await;
    // One slot: Autauga County, AL is checked against the stalled statewide layer, and Fulton
    // County, GA, which nothing claims, gets its agent all the same.
    let run = scheduler(dir.path(), &model, vec!["01001".into(), "13121".into()], false).await;
    run.settings.lock().unwrap().concurrency = 1;
    let runner = {
        let run = run.clone();
        tokio::spawn(async move { run.run().await })
    };
    let covered = async {
        loop {
            if Registry::open(&dir.path().join("registry")).unwrap().index.contains_key("13121") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(30), covered)
        .await
        .expect("Fulton County is covered while the check stalls");
    assert!(run.checks_running() >= 1, "the Alabama check is still under way");
    run.stop().await;
    runner.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_statewide_check_is_not_repeated_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    // The statewide layer has parcels only in Georgia, so it fails for Autauga County, AL.
    let root = support::serve(support::Grid::around_all(&interiors(&["13121"]))).await;
    claim_alabama(dir.path(), format!("{root}/arcgis/good/FeatureServer/0"));
    let brain: Brain = Arc::new(|_: &Request| Reply::Says("Autauga County publishes only a viewer.".into()));
    let model = Model::start(brain).await;
    let run = scheduler(dir.path(), &model, vec!["01001".into()], false).await;
    tokio::time::timeout(Duration::from_secs(60), run.run()).await.unwrap().unwrap();
    assert!(
        events(dir.path()).iter().any(|e| matches!(e, Event::Checked { adapter, fips, passed: false, .. }
            if adapter == "al-statewide" && fips == "01001")),
        "the failed check is in the journal"
    );
    drop(run);

    // Were the check made again, it would wait on the stalled server.
    let path = dir.path().join("registry/adapters/al-statewide.json");
    let text = std::fs::read_to_string(&path).unwrap().replace("/arcgis/good/", "/arcgis/stalls/");
    std::fs::write(&path, text).unwrap();
    let again = scheduler(dir.path(), &model, vec!["01001".into()], false).await;
    again.settings.lock().unwrap().retry_failed = true;
    tokio::time::timeout(Duration::from_secs(30), again.run()).await.expect("no check is repeated").unwrap();
    let checks = events(dir.path()).iter().filter(|e| matches!(e, Event::Checked { .. })).count();
    assert_eq!(checks, 1, "the restart made no check");
    let attempts = events(dir.path()).iter().filter(|e| matches!(e, Event::Started { .. })).count();
    assert_eq!(attempts, 4, "the retry climbs the ladder once more, then the run ends");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_zoning_agent_that_saved_one_layer_is_asked_once_about_the_rest_of_the_county() {
    let dir = tempfile::tempdir().unwrap();
    let root = support::serve(support::Grid::around_all(&interiors(&["55025"]))).await;
    let county = dane_zoning(&root, "wi-dane", "Dane County", "dane");
    let city = dane_zoning(&root, "wi-madison", "City of Madison", "madison");
    let brain: Brain = Arc::new(move |request: &Request| {
        let asked_again = request.user.len() > 1;
        let last = request.last_output();
        let command = |line: String| Reply::Calls("spec".into(), json!({"command": line}));
        let done = if asked_again { last.contains("Saved wi-madison") } else { last.contains("Saved ") };
        if done {
            Reply::Says("Saved.".into())
        } else if last.contains("\"passed\": true") {
            command("submit".into())
        } else {
            command(format!("validate {}", quoted(&(if asked_again { &city } else { &county }).to_string())))
        }
    });
    let model = Model::start(brain).await;
    let run = scheduler_for(dir.path(), &model, vec!["55025".into()], false, &erisgis::zoning::ZONING).await;
    tokio::time::timeout(Duration::from_secs(60), run.run()).await.unwrap().unwrap();

    let registry = Registry::open(&dir.path().join("registry")).unwrap();
    assert_eq!(registry.index["55025"], ["wi-dane", "wi-madison"]);
    let requests = model.requests.lock().unwrap();
    let asked: Vec<&String> = requests.iter().filter_map(|r| r.user.get(1)).collect();
    assert!(asked.iter().all(|m| m.contains("largest city")), "{asked:?}");
    assert_eq!(requests.last().unwrap().user.len(), 2, "asked once, not again after the second layer");
}

/// The fake server's zoning `layer` in Dane County, as `publisher`'s, saved as `id`.
fn dane_zoning(root: &str, id: &str, publisher: &str, layer: &str) -> Value {
    json!({"id": id, "publisher": publisher, "official": true,
        "source": {"kind": "arcgis-feature", "url": format!("{root}/zoning/{layer}/FeatureServer/0"), "page_size": 4},
        "coverage": {"counties": ["55025"]}, "fields": {"zone": "ZONING", "zone_name": "ZONE_DESC"}})
}

/// Saves `spec` as verified for Dane County, as an earlier run left it.
fn saved_for_dane(dir: &Path, spec: &Value) {
    let mut spec = spec.clone();
    spec["version"] = json!(1);
    let mut registry = Registry::open(&dir.join("registry")).unwrap();
    let saved = registry.save_adapter(serde_json::from_value(spec).unwrap()).unwrap();
    registry.verified(&saved.id, "55025", 1).unwrap();
}

fn jurisdiction_passes(dir: &Path) -> Vec<Outcome> {
    events(dir)
        .into_iter()
        .filter_map(|e| match e {
            Event::Finished { job: Job::Jurisdictions(fips), outcome, .. } if fips == "55025" => Some(outcome),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_covered_zoning_county_is_searched_again_for_the_jurisdictions_its_layers_leave_out() {
    let dir = tempfile::tempdir().unwrap();
    let root = support::serve(support::Grid::around_all(&interiors(&["55025"]))).await;
    saved_for_dane(dir.path(), &dane_zoning(&root, "wi-dane", "Dane County", "dane"));
    // The agent first offers the county's own layer again under a new id, then Madison's.
    let again = dane_zoning(&root, "wi-dane-zoning", "Dane County", "dane");
    let madison = dane_zoning(&root, "wi-madison", "City of Madison", "madison");
    let brain: Brain = Arc::new(move |request: &Request| {
        let command = |line: String| Reply::Calls("spec".into(), json!({"command": line}));
        let validate = |spec: &Value| command(format!("validate {}", quoted(&spec.to_string())));
        let last = request.last_output();
        let refused = request.outputs.iter().any(|o| o.contains("saved already as wi-dane"));
        if last.contains("Saved wi-madison") {
            Reply::Says("Saved wi-madison. Sun Prairie publishes its zoning only as PDF maps.".into())
        } else if last.contains("\"passed\": true") {
            command("submit".into())
        } else if refused {
            validate(&madison)
        } else {
            validate(&again)
        }
    });
    let model = Model::start(brain).await;
    let run = scheduler_for(dir.path(), &model, vec!["55025".into()], false, &erisgis::zoning::ZONING).await;
    run.settings.lock().unwrap().jurisdictions = true;
    tokio::time::timeout(Duration::from_secs(60), run.run()).await.unwrap().unwrap();
    drop(run);

    let registry = Registry::open(&dir.path().join("registry")).unwrap();
    assert_eq!(registry.index["55025"], ["wi-dane", "wi-madison"]);
    let extent = registry.adapters["wi-madison"].extent.expect("a saved layer says where it lies");
    let [x, y] = Counties::embedded().unwrap().get("55025").unwrap().interior;
    assert!(extent.contains(x, y), "{extent:?}");

    let requests = model.requests.lock().unwrap().clone();
    let first = &requests[0];
    assert!(first.instructions.contains("leave out"), "{}", first.instructions);
    assert!(first.briefing().contains("Dane County") && first.briefing().contains("wi-dane"), "{}", first.briefing());
    assert!(
        requests.iter().all(|r| !r.outputs.iter().any(|o| o.contains("Saved wi-dane-zoning"))),
        "a layer saved already is not saved twice"
    );
    let passes = jurisdiction_passes(dir.path());
    let [Outcome::Done { detail, more: false }] = passes.as_slice() else { panic!("one pass, done: {passes:?}") };
    assert!(detail.contains("wi-madison") && detail.contains("Sun Prairie"), "{detail}");

    // Done once: neither another jurisdictions run nor an ordinary one starts an agent.
    for jurisdictions in [true, false] {
        let run = scheduler_for(dir.path(), &model, vec!["55025".into()], false, &erisgis::zoning::ZONING).await;
        run.settings.lock().unwrap().jurisdictions = jurisdictions;
        tokio::time::timeout(Duration::from_secs(30), run.run()).await.unwrap().unwrap();
    }
    assert_eq!(model.requests.lock().unwrap().len(), requests.len());

    // Passed over again on request, the county's agent starts from what the last pass said.
    let run = scheduler_for(dir.path(), &model, vec!["55025".into()], false, &erisgis::zoning::ZONING).await;
    {
        let mut settings = run.settings.lock().unwrap();
        settings.jurisdictions = true;
        settings.again = true;
    }
    tokio::time::timeout(Duration::from_secs(30), run.run()).await.unwrap().unwrap();
    let requests = model.requests.lock().unwrap();
    let again = requests.last().unwrap();
    assert!(again.briefing().contains("The last pass reported") && again.briefing().contains("Sun Prairie"));
    assert!(again.briefing().contains("wi-madison"), "{}", again.briefing());
    assert_eq!(jurisdiction_passes(dir.path()).len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pass_that_names_jurisdictions_left_to_check_is_followed_by_another_up_to_a_limit_restarts_keep() {
    let dir = tempfile::tempdir().unwrap();
    let root = support::serve(support::Grid::around_all(&interiors(&["55025"]))).await;
    saved_for_dane(dir.path(), &dane_zoning(&root, "wi-dane", "Dane County", "dane"));
    // Every pass runs out of room before Sun Prairie, and says so, as a list with commas.
    let brain: Brain = Arc::new(|request: &Request| {
        if request.outputs.is_empty() {
            Reply::Calls("spec".into(), json!({"command": "left Sun Prairie, Fitchburg"}))
        } else {
            Reply::Says("Sun Prairie and Fitchburg are left to check.".into())
        }
    });
    let model = Model::start(brain).await;
    let run = scheduler_for(dir.path(), &model, vec!["55025".into()], false, &erisgis::zoning::ZONING).await;
    run.settings.lock().unwrap().jurisdictions = true;
    tokio::time::timeout(Duration::from_secs(90), run.run()).await.unwrap().unwrap();

    drop(run);
    let passes = jurisdiction_passes(dir.path());
    assert_eq!(passes.len(), PASSES_IN_A_ROW, "{passes:?}");
    let Outcome::Done { detail, more: true } = &passes[0] else { panic!("{passes:?}") };
    assert!(detail.contains("Left to check: Sun Prairie, Fitchburg."), "{detail}");
    let asked = model.requests.lock().unwrap().len();
    assert!(
        model.requests.lock().unwrap().iter().any(|r| r.briefing().contains("Left to check: Sun Prairie, Fitchburg"))
    );

    // A restarted run keeps the count: no sixth pass.
    let run = scheduler_for(dir.path(), &model, vec!["55025".into()], false, &erisgis::zoning::ZONING).await;
    run.settings.lock().unwrap().jurisdictions = true;
    tokio::time::timeout(Duration::from_secs(30), run.run()).await.unwrap().unwrap();
    assert_eq!(model.requests.lock().unwrap().len(), asked);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pass_that_saves_as_many_layers_as_a_pass_may_is_followed_by_another() {
    let dir = tempfile::tempdir().unwrap();
    let root = support::serve(support::Grid::around_all(&interiors(&["55025"]))).await;
    saved_for_dane(dir.path(), &dane_zoning(&root, "wi-dane", "Dane County", "dane"));
    let towns: Vec<Value> = (1..=LAYERS_PER_PASS)
        .map(|n| dane_zoning(&root, &format!("wi-town-{n}"), &format!("Town {n}"), &format!("town-{n}")))
        .collect();
    let brain: Brain = Arc::new(move |request: &Request| {
        if request.briefing().contains("The last pass") {
            return Reply::Says("No jurisdiction is left.".into());
        }
        let command = |line: String| Reply::Calls("spec".into(), json!({"command": line}));
        let saved = request.outputs.iter().filter(|o| o.starts_with("Saved ")).count();
        let last = request.last_output();
        if last.starts_with("Noted") {
            Reply::Says("Saved eight towns; Town 9 and Town 10 are left to check.".into())
        } else if last.contains("the most one pass saves") {
            command("left \"Town 9\" \"Town 10\"".into())
        } else if last.contains("\"passed\": true") {
            command("submit".into())
        } else {
            command(format!("validate {}", quoted(&towns[saved].to_string())))
        }
    });
    let model = Model::start(brain).await;
    let run = scheduler_for(dir.path(), &model, vec!["55025".into()], false, &erisgis::zoning::ZONING).await;
    run.settings.lock().unwrap().jurisdictions = true;
    tokio::time::timeout(Duration::from_secs(90), run.run()).await.unwrap().unwrap();

    let registry = Registry::open(&dir.path().join("registry")).unwrap();
    assert_eq!(registry.index["55025"].len(), 1 + LAYERS_PER_PASS);
    let passes = jurisdiction_passes(dir.path());
    assert!(
        matches!(passes.as_slice(), [Outcome::Done { more: true, .. }, Outcome::Done { more: false, .. }]),
        "{passes:?}"
    );
    let requests = model.requests.lock().unwrap();
    let second = requests.iter().find(|r| r.briefing().contains("The last pass")).unwrap();
    assert!(second.briefing().contains("Left to check: Town 9, Town 10"), "{}", second.briefing());
    assert!(second.briefing().contains("wi-town-8"), "the saved layers include the last pass's");
}
