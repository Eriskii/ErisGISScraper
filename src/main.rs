//! `eris-gis-scraper`: runs agents over US counties to write a dataset's adapters, validates
//! specs, signs in, and reports where the registry stands.

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use erisagentauth::claude;
use erisagentauth::openai::{self, Method};
use erisagentauth::{Accounts, App, Event};
use erisgis::counties::{self, Counties};
use erisgis::http::{Http, Limits};
use erisgis::registry::{Registry, Standing, now};
use erisgis::runtime::{Runtime, VALIDATE_WITHIN};
use erisgis::spec::Spec;
use erisgis::{Dataset, validate};
use erisgisscraper::journal::Journal;
use erisgisscraper::models::{ANTHROPIC, CODEX, Providers};
use erisgisscraper::scheduler::{Scheduler, Settings};
use erisgisscraper::view::View;
use erisgisscraper::work::Work;
use erisgisscraper::{models, tools, tui};
use erisharness::Harness;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP: App = App { name: "ErisGISScraper", originator: "erisgisscraper" };

#[derive(Parser)]
#[command(name = "eris-gis-scraper", about = "Write and validate an adapter of an ErisGIS dataset for every US county")]
struct Cli {
    /// The dataset: parcels or zoning.
    #[arg(long, global = true, default_value = "parcels", value_parser = clap::builder::PossibleValuesParser::new(erisgis::dataset::ALL.map(|d| d.name)))]
    dataset: String,
    /// The registries: ErisData's `GIS/`, checked out beside this repository in the monorepo.
    /// Each dataset's is the folder named after it.
    #[arg(long, global = true, default_value = "../../data/ErisData/GIS")]
    registry: PathBuf,
    /// Run state: the logins and downloads, and each dataset's journal and agents in a folder
    /// named after it. Kept out of the repository.
    #[arg(long, global = true, default_value = "state")]
    state: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run agents over the selected counties (every county when none are named).
    Run {
        /// States by postal abbreviation, such as WI,GA.
        #[arg(long, value_delimiter = ',')]
        states: Vec<String>,
        /// Counties by FIPS, such as 13121.
        #[arg(long, value_delimiter = ',')]
        counties: Vec<String>,
        /// Jobs running at once.
        #[arg(long, default_value_t = 8)]
        concurrency: usize,
        /// Models cheapest first, as model[:effort],…, such as haiku:medium,sonnet:high; haiku,
        /// sonnet and opus mean the newest Claude model of that family. By default: with a Claude
        /// login, Haiku at medium, Sonnet at high, then ChatGPT's flagship (or Opus) at high; with
        /// ChatGPT alone, its small model at medium, then its flagship at medium and high.
        #[arg(long)]
        ladder: Option<String>,
        /// Attempts on each rung before climbing to the next.
        #[arg(long, default_value_t = 1)]
        attempts_per_rung: usize,
        /// Failed validations an attempt may make.
        #[arg(long, default_value_t = 8)]
        validate_budget: usize,
        /// Skip the statewide search and go straight to counties.
        #[arg(long)]
        no_statewide: bool,
        /// Validate every saved adapter first; counties whose adapter fails are searched again.
        #[arg(long)]
        recheck: bool,
        /// Try counties again that used up the ladder.
        #[arg(long)]
        retry_failed: bool,
        /// Pass over the selected counties that are covered, for the jurisdictions their saved
        /// layers leave out (cities' zoning beside the county's, say), instead of searching open
        /// ones: one pass each, and another after a pass that saves as many layers as one may.
        /// For a dataset taken per jurisdiction.
        #[arg(long)]
        jurisdictions: bool,
        /// With --jurisdictions: pass over counties passed over before, too, each briefed with
        /// what its last pass said.
        #[arg(long, requires = "jurisdictions")]
        again: bool,
        /// Print progress lines instead of drawing the terminal view.
        #[arg(long)]
        no_tui: bool,
    },
    /// Validate a spec file of the dataset for a county and print the report.
    Validate { spec: PathBuf, fips: String },
    /// Sign in with ChatGPT, or bind a Claude login with --claude.
    Login {
        /// Use a device code instead of the browser.
        #[arg(long, conflicts_with = "claude")]
        device: bool,
        /// Bind Claude Code's credentials file (by default $CLAUDE_CONFIG_DIR/.credentials.json,
        /// or ~/.claude/.credentials.json). The login is read from that file on every run and
        /// never refreshed here, so Claude Code stays signed in; it lasts until another
        /// `login --claude`. When the file's token has expired, open Claude Code to refresh it.
        #[arg(long, value_name = "FILE", num_args = 0..=1)]
        claude: Option<Option<PathBuf>>,
    },
    /// Use the ChatGPT login in a credential file another Eris app wrote.
    ImportLogin { file: PathBuf },
    /// List the signed-in logins' models and the default ladder.
    Models,
    /// Where the dataset's registry stands, by state.
    Status,
    /// Field notes from the run, as Markdown: unavailable counties by reason with their
    /// evidence, what stopped agents grouped by cause, the hosts behind the most trouble, and
    /// what the run cost.
    Notes {
        /// Write them to this file instead of printing them.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Rebuild the embedded county file from census.gov.
    RegenerateCounties {
        #[arg(default_value = "erisgis/data/counties.json.gz")]
        out: PathBuf,
    },
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.into())
}

/// The ChatGPT and Claude logins kept owner-only in the run state's `accounts/`, announcing on
/// one channel. ERISGISSCRAPER_OPENAI_AUTH, ERISGISSCRAPER_CLAUDE_API and
/// ERISGISSCRAPER_CLAUDE_AUTH point them at other servers.
fn accounts(state: &Path) -> Result<Accounts> {
    Accounts::load_from(
        &state.join("accounts"),
        APP,
        &env_or("ERISGISSCRAPER_OPENAI_AUTH", openai::AUTH),
        &env_or("ERISGISSCRAPER_CLAUDE_API", claude::API),
        &env_or("ERISGISSCRAPER_CLAUDE_AUTH", claude::AUTH),
    )
}

/// The Codex endpoint, or ERISGISSCRAPER_CODEX_API.
fn codex_api() -> String {
    env_or("ERISGISSCRAPER_CODEX_API", openai::CODEX_API)
}

/// Every signed-in login's models: Claude's, then ChatGPT's. A login whose listing fails is
/// reported and left out.
async fn listed_models(accounts: &Accounts) -> Vec<models::Model> {
    let mut listed = Vec::new();
    match accounts.claude.models().await {
        Ok(models) => listed.extend(models),
        Err(error) => eprintln!("listing Claude models failed: {error:#}"),
    }
    match accounts.openai.models(&codex_api()).await {
        Ok(models) => listed.extend(models),
        Err(error) => eprintln!("listing ChatGPT models failed: {error:#}"),
    }
    listed
}

fn runtime(state: &Path) -> Result<Runtime> {
    Ok(Runtime {
        http: Http::new(Limits::default())?,
        cache: state.join("downloads"),
        validate_within: VALIDATE_WITHIN,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let dataset = Dataset::named(&cli.dataset).expect("a listed dataset");
    let registry_dir = cli.registry.join(dataset.name);
    let run_state = cli.state.join(dataset.name);
    match cli.command {
        Command::Validate { spec, fips } => {
            let counties = Counties::embedded()?;
            let county = counties.get(&fips).ok_or_else(|| anyhow!("no county {fips}"))?;
            let spec: Spec = serde_json::from_slice(&std::fs::read(&spec)?).context("reading the spec")?;
            let report = validate::validate(&runtime(&cli.state)?, dataset, &spec, county).await;
            println!("{}", serde_json::to_string_pretty(&report)?);
            if !report.passed {
                std::process::exit(1);
            }
        }
        Command::Login { claude: Some(path), .. } => {
            let path = match path {
                Some(path) => path,
                None => claude::claude_code_credentials()?,
            };
            let accounts = accounts(&cli.state)?;
            let snapshot = accounts.claude.bind(&path).await?;
            println!(
                "Bound the Claude login of {} at {}. Runs read it from there and never refresh it.",
                snapshot.email.as_deref().unwrap_or("(unknown)"),
                path.display()
            );
        }
        Command::Login { device, claude: None } => {
            let accounts = accounts(&cli.state)?;
            let mut events = accounts.subscribe();
            let start = accounts.openai.login(if device { Method::DeviceCode } else { Method::Browser }).await?;
            println!("Open {}", start.url);
            if let Some(code) = start.user_code {
                println!("and enter the code {code}");
            }
            while let Ok(event) = events.recv().await {
                if let Event::LoginCompleted { ok, error, .. } = event {
                    if ok {
                        println!("Signed in.");
                        return Ok(());
                    }
                    bail!("signing in failed: {}", error.unwrap_or_default());
                }
            }
        }
        Command::ImportLogin { file } => {
            let credential: openai::Credential =
                serde_json::from_slice(&std::fs::read(&file)?).context("the file is not a ChatGPT credential")?;
            // Loading the accounts makes their owner-only folder.
            accounts(&cli.state)?;
            let path = cli.state.join("accounts").join("chatgpt.json");
            let text = serde_json::to_vec_pretty(&serde_json::json!({"version": 1, "credential": credential}))?;
            erisgis::registry::write_atomic(&path, &text)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            println!("Imported the login of {}.", credential.email.as_deref().unwrap_or("(unknown)"));
        }
        Command::Models => {
            let accounts = accounts(&cli.state)?;
            let listed = listed_models(&accounts).await;
            if listed.is_empty() {
                bail!("no models: sign in first (eris-gis-scraper login, or login --claude)");
            }
            for m in &listed {
                let efforts = match &m.cannot_run {
                    Some(reason) => format!("cannot run: {reason}"),
                    None => format!("efforts {}", m.efforts.join(",")),
                };
                println!("{:10} {:28} context {:>8} {efforts}", m.provider, m.id, m.context_window.unwrap_or(0));
            }
            println!("\ndefault ladder:");
            for (i, rung) in models::default_ladder(&listed).iter().enumerate() {
                println!("  {i}: {}", rung.label());
            }
        }
        Command::Status => status(&registry_dir)?,
        Command::Notes { out } => {
            let counties = Counties::embedded()?;
            let registry = Registry::open(&registry_dir)?;
            let events = Journal::read(&run_state.join("journal.jsonl"))?;
            let text = erisgisscraper::notes::notes(&counties, &registry, &events, now());
            match out {
                Some(path) => {
                    std::fs::write(&path, text)?;
                    println!("wrote {}", path.display());
                }
                None => print!("{text}"),
            }
        }
        Command::RegenerateCounties { out } => {
            let count = counties::regenerate(&out).await?;
            println!("wrote {count} counties to {}", out.display());
        }
        Command::Run {
            states,
            counties: fips,
            concurrency,
            ladder,
            attempts_per_rung,
            validate_budget,
            no_statewide,
            recheck,
            retry_failed,
            jurisdictions,
            again,
            no_tui,
        } => {
            if jurisdictions && !dataset.per_jurisdiction {
                bail!("--jurisdictions is for a dataset taken per jurisdiction, which {} is not", dataset.name);
            }
            let all = Arc::new(Counties::embedded()?);
            let selected = select(&all, &states, &fips)?;
            // Naming counties alone skips the statewide search unless states are named too.
            let statewide = !no_statewide && !jurisdictions && (fips.is_empty() || !states.is_empty());
            let accounts = accounts(&cli.state)?;
            let listed = listed_models(&accounts).await;
            let ladder = match &ladder {
                Some(text) => models::parse_ladder(text, &listed)?,
                None => models::default_ladder(&listed),
            };
            if ladder.is_empty() {
                bail!("no model ladder: sign in first (eris-gis-scraper login, or login --claude), or pass --ladder");
            }
            let providers = Providers::new(&accounts.openai, &accounts.claude, &codex_api()).await?;
            let web = providers.web(&listed);
            let work = Arc::new(Work {
                dataset,
                counties: all.clone(),
                registry: Mutex::new(Registry::open(&registry_dir)?),
                runtime: runtime(&cli.state)?,
                web,
                web_http: erisharness::tools::web::Http::new(Default::default(), erisgis::USER_AGENT)?,
                agents: Mutex::new(Default::default()),
                validate_budget,
                log: Mutex::new(Default::default()),
                logged: Default::default(),
            });
            let harness_dir = run_state.join("harness");
            let empty = harness_dir.join("empty");
            std::fs::create_dir_all(&empty)?;
            let mut builder = Harness::builder(&harness_dir);
            for (name, provider) in providers.named() {
                builder = builder.provider(name, provider);
            }
            let harness = builder
                .tools(tools::all(&work))
                .max_concurrent_turns(256)
                .open()
                .await
                .context("opening the agents' state")?;
            let (journal, past) = Journal::open(&run_state.join("journal.jsonl"))?;
            let settings =
                Settings { concurrency, ladder, attempts_per_rung, statewide, retry_failed, jurisdictions, again };
            // Jobs start while every login the ladder runs on gives a token.
            let needs_codex = settings.ladder.iter().any(|r| r.provider == CODEX);
            let needs_claude = settings.ladder.iter().any(|r| r.provider == ANTHROPIC);
            let (openai, claude) = (accounts.openai.clone(), accounts.claude.clone());
            let ready: Box<dyn Fn() -> futures_util::future::BoxFuture<'static, bool> + Send + Sync> =
                Box::new(move || {
                    let (openai, claude) = (openai.clone(), claude.clone());
                    Box::pin(async move {
                        (!needs_codex || openai.current().await.is_ok())
                            && (!needs_claude || claude.current().await.is_ok())
                    })
                });
            let scheduler = Scheduler::new(work, harness, journal, past, settings, selected, empty, ready);
            if recheck {
                scheduler.recheck().await;
            }
            if no_tui {
                headless(scheduler).await?;
            } else {
                let events = accounts.subscribe();
                tui::run(scheduler, accounts.openai, accounts.claude, events).await?;
            }
        }
    }
    Ok(())
}

fn status(registry: &Path) -> Result<()> {
    let counties = Counties::embedded()?;
    let registry = Registry::open(registry)?;
    let mut by_state: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    let at = now();
    for county in &counties.list {
        let entry = by_state.entry(county.state.clone()).or_default();
        entry.0 += 1;
        match registry.standing(&county.fips, at) {
            Standing::Covered(_) => entry.1 += 1,
            Standing::Unavailable(_) => entry.2 += 1,
            Standing::Open => {}
        }
    }
    let (mut total, mut covered, mut unavailable) = (0, 0, 0);
    println!("state  counties  covered  unavailable");
    for (state, (n, c, u)) in &by_state {
        println!("{state:5}  {n:8}  {c:7}  {u:11}");
        total += n;
        covered += c;
        unavailable += u;
    }
    println!("all    {total:8}  {covered:7}  {unavailable:11}   ({} adapters)", registry.adapters.len());
    Ok(())
}

/// The selected counties: those named, every county of the states named, or every county.
fn select(all: &Counties, states: &[String], fips: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for state in states {
        let state = state.to_uppercase();
        let before = out.len();
        out.extend(all.in_state(&state).map(|c| c.fips.clone()));
        if out.len() == before {
            bail!("no counties in {state}");
        }
    }
    for f in fips {
        if all.get(f).is_none() {
            bail!("no county {f}");
        }
        out.push(f.clone());
    }
    if states.is_empty() && fips.is_empty() {
        out = all.list.iter().map(|c| c.fips.clone()).collect();
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Prints the log as it grows and a progress line every 15 seconds, until the run settles or
/// Ctrl-C stops it.
async fn headless(scheduler: Arc<Scheduler>) -> Result<()> {
    let printed = Arc::new(Mutex::new(0u64));
    let printer = {
        let (scheduler, printed) = (scheduler.clone(), printed.clone());
        tokio::spawn(async move {
            let mut tick = 0u64;
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                print_new(&scheduler, &mut printed.lock().unwrap());
                tick += 1;
                if tick.is_multiple_of(15) {
                    println!("· {}", View::of(&scheduler).progress_line());
                }
            }
        })
    };
    let runner = {
        let scheduler = scheduler.clone();
        tokio::spawn(async move { scheduler.run().await })
    };
    tokio::select! {
        result = runner => { result??; }
        _ = tokio::signal::ctrl_c() => { println!("stopping; agents resume on the next run"); }
    }
    printer.abort();
    print_new(&scheduler, &mut printed.lock().unwrap());
    println!("{}", View::of(&scheduler).progress_line());
    scheduler.stop().await;
    Ok(())
}

fn print_new(scheduler: &Scheduler, printed: &mut u64) {
    let log = scheduler.work.log.lock().unwrap();
    let logged = scheduler.work.logged.load(Ordering::SeqCst);
    let new = (logged - *printed).min(log.len() as u64) as usize;
    for line in log.iter().skip(log.len() - new) {
        println!("{line}");
    }
    *printed = logged;
}
