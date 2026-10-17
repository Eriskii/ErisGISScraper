//! What the scheduler and the agents' tools share: the dataset, the counties, its registry, the
//! runtime, and each live agent's job and progress.

use crate::journal::{Job, Tokens};
use erisgis::Dataset;
use erisgis::counties::{Counties, County};
use erisgis::registry::Registry;
use erisgis::runtime::Runtime;
use erisgis::spec::{Reason, Spec};
use erisgis::validate::{self, Report};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// How an agent's job ended, as its tools recorded it.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    /// The county is covered by this adapter; for a state job, the statewide adapter.
    Covered(String),
    Unavailable(Reason),
}

#[derive(Clone)]
pub struct Validated {
    pub spec: Spec,
    pub passed: bool,
    /// The reports as the agent read them.
    pub report: String,
}

/// One live agent's job.
pub struct AgentJob {
    pub job: Job,
    pub rung: usize,
    pub model: String,
    /// Validations that failed.
    pub failures: usize,
    /// The spec validated last, whether it passed, and its report.
    pub last: Option<Validated>,
    pub resolution: Option<Resolution>,
    /// The tool call running or last run, for the TUI.
    pub activity: String,
    pub tokens: Tokens,
    /// Tokens the agent's tools spent on models of their own, by command; counted in `tokens`.
    pub tool_tokens: BTreeMap<String, Tokens>,
    /// What the attempt has tried, newest last, for the next rung's briefing.
    pub trail: VecDeque<String>,
    pub started: u64,
    /// Whether the agent has been told to keep going once already.
    pub nudged: bool,
    /// Specs it saved.
    pub saved: usize,
    /// Whether it has been asked about the jurisdictions its saved layer leaves out.
    pub followed_up: bool,
}

impl AgentJob {
    pub fn new(job: Job, rung: usize, model: String) -> Self {
        Self {
            job,
            rung,
            model,
            failures: 0,
            last: None,
            resolution: None,
            activity: "starting".into(),
            tokens: Tokens::default(),
            tool_tokens: BTreeMap::new(),
            trail: VecDeque::new(),
            started: erisgis::registry::now(),
            nudged: false,
            saved: 0,
            followed_up: false,
        }
    }

    pub fn note(&mut self, line: String) {
        if self.trail.len() >= 12 {
            self.trail.pop_front();
        }
        self.trail.push_back(line);
    }
}

pub struct Work {
    /// The dataset the run writes adapters for.
    pub dataset: &'static Dataset,
    pub counties: Arc<Counties>,
    pub registry: Mutex<Registry>,
    pub runtime: Runtime,
    /// The models `web` searches and reads pages with, and the client it reads the web with.
    pub web: erisharness::tools::web::Models,
    pub web_http: erisharness::tools::web::Http,
    pub agents: Mutex<HashMap<String, AgentJob>>,
    /// Failed validations an attempt may make before it must stop.
    pub validate_budget: usize,
    pub log: Mutex<VecDeque<String>>,
    /// Lines logged since the start, including those the log no longer keeps.
    pub logged: std::sync::atomic::AtomicU64,
}

impl Work {
    pub fn log(&self, line: impl Into<String>) {
        let mut log = self.log.lock().unwrap();
        if log.len() >= 500 {
            log.pop_front();
        }
        log.push_back(line.into());
        self.logged.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Counts `tokens` a tool's own model call spent toward `agent`'s job, under `command`.
    pub fn charge(&self, agent: &str, command: &str, tokens: &Tokens) {
        self.with_agent(agent, |a| {
            a.tokens.add(tokens);
            a.tool_tokens.entry(command.to_owned()).or_default().add(tokens);
        });
    }

    pub fn with_agent<T>(&self, agent: &str, f: impl FnOnce(&mut AgentJob) -> T) -> Option<T> {
        self.agents.lock().unwrap().get_mut(agent).map(f)
    }

    /// The counties a spec is validated against for a job: the county, or for a state, three
    /// spread across it.
    pub fn sample_counties(&self, job: &Job) -> Vec<County> {
        match job {
            Job::County(fips) => self.counties.get(fips).into_iter().cloned().collect(),
            Job::State(state) => self.counties.spread(state, 3).into_iter().cloned().collect(),
        }
    }

    /// Validates `spec` against each sample county of `job`, all at once. A state's spec passes when at
    /// least two of three samples pass, since statewide layers sometimes miss a county.
    pub async fn validate_for(&self, job: &Job, spec: &Spec) -> (bool, Vec<Report>) {
        let counties = self.sample_counties(job);
        let reports: Vec<Report> = futures_util::future::join_all(
            counties.iter().map(|county| validate::validate(&self.runtime, self.dataset, spec, county)),
        )
        .await;
        let passed = reports.iter().filter(|r| r.passed).count();
        let ok = match job {
            Job::County(_) => passed == 1,
            Job::State(_) => passed >= counties.len().min(2),
        };
        (ok, reports)
    }
}
