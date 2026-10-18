//! A snapshot of a run for display: totals, each state, live agents, the ladder's rungs, and
//! the log's tail. The TUI draws it; headless runs print its progress line.

use crate::journal::{Job, Tokens};
use crate::scheduler::{Scheduler, Status};
use erisgis::registry::now;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug, Default)]
pub struct Counts {
    pub covered: usize,
    pub unavailable: usize,
    pub running: usize,
    pub failed: usize,
    pub pending: usize,
}

impl Counts {
    fn add(&mut self, status: Status) {
        match status {
            Status::Covered => self.covered += 1,
            Status::Unavailable => self.unavailable += 1,
            Status::Running => self.running += 1,
            Status::Failed => self.failed += 1,
            Status::Pending => self.pending += 1,
        }
    }

    pub fn total(&self) -> usize {
        self.covered + self.unavailable + self.running + self.failed + self.pending
    }
}

pub struct AgentRow {
    pub job: String,
    pub rung: usize,
    pub model: String,
    pub activity: String,
    pub seconds: u64,
    pub tokens: Tokens,
    pub failures: usize,
}

pub struct RungRow {
    pub rung: usize,
    pub label: String,
    pub attempts: usize,
    pub successes: usize,
    pub tokens: Tokens,
}

pub struct View {
    pub totals: Counts,
    /// Each state's counts, and whether its statewide search is running.
    pub states: Vec<(String, Counts, bool)>,
    pub agents: Vec<AgentRow>,
    pub rungs: Vec<RungRow>,
    pub log: Vec<String>,
    pub paused: bool,
    pub concurrency: usize,
    /// Counties whose claims are being checked, beside the agents.
    pub checks: usize,
    pub tokens: Tokens,
}

impl View {
    pub fn of(scheduler: &Scheduler) -> Self {
        let mut totals = Counts::default();
        let mut states: BTreeMap<String, Counts> = BTreeMap::new();
        for fips in &scheduler.counties {
            let status = scheduler.status(fips);
            totals.add(status);
            if let Some(county) = scheduler.work.counties.get(fips) {
                states.entry(county.state.clone()).or_default().add(status);
            }
        }
        let at = now();
        let mut agents: Vec<AgentRow> = scheduler
            .work
            .agents
            .lock()
            .unwrap()
            .values()
            .map(|a| AgentRow {
                job: match &a.job {
                    Job::County(fips) => scheduler.work.counties.get(fips).map_or(fips.clone(), |c| c.label()),
                    Job::Jurisdictions(fips) => format!(
                        "{} (jurisdictions)",
                        scheduler.work.counties.get(fips).map_or(fips.clone(), |c| c.label())
                    ),
                    Job::State(state) => format!("statewide {state}"),
                },
                rung: a.rung,
                model: a.model.clone(),
                activity: a.activity.clone(),
                seconds: at.saturating_sub(a.started) / 1000,
                tokens: a.tokens.clone(),
                failures: a.failures,
            })
            .collect();
        agents.sort_by_key(|a| std::cmp::Reverse(a.seconds));
        let ladder = scheduler.settings.lock().unwrap().ladder.clone();
        let stats = scheduler.rungs.lock().unwrap().clone();
        let mut tokens = Tokens::default();
        let rungs = (0..ladder.len().max(stats.keys().max().map_or(0, |m| m + 1)))
            .map(|rung| {
                let s = stats.get(&rung).cloned().unwrap_or_default();
                tokens.add(&s.tokens);
                RungRow {
                    rung,
                    label: ladder.get(rung).map_or("(not on the ladder)".into(), |r| r.label()),
                    attempts: s.attempts,
                    successes: s.successes,
                    tokens: s.tokens,
                }
            })
            .collect();
        for agent in &agents {
            tokens.add(&agent.tokens);
        }
        let running_states: Vec<String> = scheduler
            .work
            .agents
            .lock()
            .unwrap()
            .values()
            .filter_map(|a| match &a.job {
                Job::State(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        let log = scheduler.work.log.lock().unwrap().iter().rev().take(200).cloned().collect();
        Self {
            totals,
            states: states.into_iter().map(|(s, c)| (s.clone(), c, running_states.contains(&s))).collect(),
            agents,
            rungs,
            log,
            paused: scheduler.paused.load(Ordering::SeqCst),
            concurrency: scheduler.settings.lock().unwrap().concurrency,
            checks: scheduler.checks_running(),
            tokens,
        }
    }

    pub fn progress_line(&self) -> String {
        let t = &self.totals;
        format!(
            "{} counties: {} covered, {} unavailable, {} running, {} failed, {} pending · tokens {} in ({} cached) / {} out",
            t.total(),
            t.covered,
            t.unavailable,
            t.running,
            t.failed,
            t.pending,
            human(self.tokens.input),
            human(self.tokens.cached_input),
            human(self.tokens.output)
        )
    }
}

pub fn human(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.2}B", n as f64 / 1e9),
    }
}
