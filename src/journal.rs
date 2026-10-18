//! The run journal: one JSON line per event, appended and flushed as it happens. Replaying it
//! gives each job's attempts, its rung on the model ladder, the agent it has running, and what
//! earlier attempts tried, so a killed run resumes where it stood.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "key", rename_all = "snake_case")]
pub enum Job {
    /// The search for one layer covering a whole state, by postal abbreviation.
    State(String),
    /// The search for one county's layers, by FIPS.
    County(String),
    /// In a dataset taken per jurisdiction, a pass over a covered county, by FIPS, for the
    /// jurisdictions its saved layers leave out.
    Jurisdictions(String),
}

impl Job {
    pub fn label(&self) -> String {
        match self {
            Job::State(state) => format!("state {state}"),
            Job::County(fips) => format!("county {fips}"),
            Job::Jurisdictions(fips) => format!("county {fips} (jurisdictions)"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// An adapter now covers the county, or for a state job or a jurisdictions pass, the search
    /// ended. `more`: the pass saved as many layers as one may, so another is due.
    Done {
        detail: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        more: bool,
    },
    /// The county was recorded as unavailable.
    Unavailable { reason: String },
    /// The agent ran out of validation attempts or stopped without a result.
    GaveUp { summary: String },
    /// The agent's turn failed, such as on a provider error.
    Failed { error: String },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub cached_input: u64,
    pub output: u64,
}

impl Tokens {
    pub fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.cached_input += other.cached_input;
        self.output += other.output;
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Started {
        at: u64,
        job: Job,
        rung: usize,
        model: String,
        agent: String,
    },
    Finished {
        at: u64,
        job: Job,
        rung: usize,
        agent: String,
        outcome: Outcome,
        /// Every token the attempt spent: its agent's and its tools' own model calls.
        tokens: Tokens,
        /// Tokens the agent's tools spent on models of their own, by command (`web search`,
        /// `web read`); counted in `tokens` too.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        tools: BTreeMap<String, Tokens>,
    },
    /// An adapter claiming a county it was not verified for was checked there, with no agent.
    Checked {
        at: u64,
        adapter: String,
        fips: String,
        passed: bool,
    },
}

/// A job as the journal tells it.
#[derive(Clone, Debug, Default)]
pub struct JobHistory {
    /// Attempts started on each rung.
    pub attempts: BTreeMap<usize, usize>,
    /// The attempt in progress: its agent, rung and model.
    pub running: Option<(String, usize, String)>,
    pub finished: Option<Outcome>,
    /// What earlier attempts that gave up tried.
    pub summaries: Vec<String>,
    pub tokens: Tokens,
    /// The highest rung started.
    pub rung: usize,
    /// Results in a row that asked for another pass (a jurisdictions pass's `more`).
    pub streak: usize,
}

impl JobHistory {
    /// Counts a result toward the streak of those asking for another pass.
    pub fn note(&mut self, outcome: &Outcome) {
        match outcome {
            Outcome::Done { more: true, .. } => self.streak += 1,
            Outcome::Done { more: false, .. } | Outcome::Unavailable { .. } => self.streak = 0,
            Outcome::GaveUp { .. } | Outcome::Failed { .. } => {}
        }
    }
}

pub struct Journal {
    path: PathBuf,
    file: Mutex<std::fs::File>,
}

impl Journal {
    /// The events at `path`, without writing to it, so a running journal can be read. A line
    /// cut short, by a kill or by a write under way, is the only one that fails to read; it is
    /// skipped.
    pub fn read(path: &Path) -> Result<Vec<Event>> {
        let mut events = Vec::new();
        if let Ok(file) = std::fs::File::open(path) {
            for line in std::io::BufReader::new(file).lines() {
                if let Ok(event) = serde_json::from_str::<Event>(&line?) {
                    events.push(event);
                }
            }
        }
        Ok(events)
    }

    pub fn open(path: &Path) -> Result<(Self, Vec<Event>)> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let events = Self::read(path)?;
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        // End a line cut short by a kill, so the next event starts a line of its own.
        let bytes = std::fs::read(path)?;
        if bytes.last().is_some_and(|&b| b != b'\n') {
            file.write_all(b"\n")?;
        }
        Ok((Self { path: path.to_owned(), file: Mutex::new(file) }, events))
    }

    pub fn append(&self, event: &Event) -> Result<()> {
        let mut line = serde_json::to_string(event)?;
        line.push('\n');
        let mut file = self.file.lock().unwrap();
        file.write_all(line.as_bytes())?;
        file.sync_data()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Each job's history from the journal's events.
pub fn replay(events: &[Event]) -> BTreeMap<Job, JobHistory> {
    let mut jobs: BTreeMap<Job, JobHistory> = BTreeMap::new();
    for event in events {
        match event {
            Event::Started { job, rung, agent, model, .. } => {
                let history = jobs.entry(job.clone()).or_default();
                *history.attempts.entry(*rung).or_default() += 1;
                history.running = Some((agent.clone(), *rung, model.clone()));
                history.finished = None;
                history.rung = history.rung.max(*rung);
            }
            Event::Finished { job, outcome, tokens, .. } => {
                let history = jobs.entry(job.clone()).or_default();
                history.running = None;
                history.tokens.add(tokens);
                match outcome {
                    Outcome::GaveUp { summary } => history.summaries.push(summary.clone()),
                    Outcome::Failed { error } => history.summaries.push(format!("The attempt failed: {error}")),
                    // A result ends the job's cycle: a later recheck starts it again from the
                    // bottom of the ladder.
                    Outcome::Done { .. } | Outcome::Unavailable { .. } => {
                        history.attempts.clear();
                        history.summaries.clear();
                        history.rung = 0;
                    }
                }
                history.note(outcome);
                history.finished = Some(outcome.clone());
            }
            Event::Checked { .. } => {}
        }
    }
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_survives_a_cut_line_and_reports_running_agents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.jsonl");
        let (journal, _) = Journal::open(&path).unwrap();
        let job = Job::County("13121".into());
        journal
            .append(&Event::Started { at: 1, job: job.clone(), rung: 0, model: "mini".into(), agent: "a1".into() })
            .unwrap();
        journal
            .append(&Event::Finished {
                at: 2,
                job: job.clone(),
                rung: 0,
                agent: "a1".into(),
                outcome: Outcome::GaveUp { summary: "tried the county portal".into() },
                tokens: Tokens { input: 10, cached_input: 0, output: 5 },
                tools: BTreeMap::new(),
            })
            .unwrap();
        journal
            .append(&Event::Started { at: 3, job: job.clone(), rung: 1, model: "big".into(), agent: "a2".into() })
            .unwrap();
        drop(journal);
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"event\":\"fini").unwrap();

        let (journal, events) = Journal::open(&path).unwrap();
        journal
            .append(&Event::Started {
                at: 4,
                job: Job::State("GA".into()),
                rung: 0,
                model: "mini".into(),
                agent: "a3".into(),
            })
            .unwrap();
        assert_eq!(Journal::open(&path).unwrap().1.len(), events.len() + 1, "the event after a cut line reads");
        let jobs = replay(&events);
        let history = &jobs[&job];
        assert_eq!(history.running, Some(("a2".into(), 1, "big".into())));
        assert_eq!(history.rung, 1);
        assert_eq!(history.summaries, ["tried the county portal"]);
        assert_eq!(history.tokens.input, 10);
        assert_eq!(history.attempts.values().sum::<usize>(), 2);
    }
}
