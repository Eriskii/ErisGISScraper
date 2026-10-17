//! The agents' tools: three command lines run in-process, like a shell without one. `web`
//! (ErisHarness's) reads the public web through its polite client, `arcgis` searches for layers
//! and inspects services, and `spec` validates specs and saves results. There is no shell, no
//! file system, and no way into the registry except `spec submit`, which saves only a spec that
//! passed.

mod arcgis;
mod spec;
pub mod text;

pub use arcgis::Arcgis;
pub use spec::{STATEWIDE_SPEC, SpecTool};

use crate::journal::Tokens;
use crate::work::Work;
use erisharness::tools::Tool;
use erisharness::tools::web::Web;
use std::sync::Arc;

pub fn all(work: &Arc<Work>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(web(work)),
        Arc::new(Arcgis(work.clone())),
        Arc::new(SpecTool { work: work.clone(), statewide: false }),
        Arc::new(SpecTool { work: work.clone(), statewide: true }),
    ]
}

/// ErisHarness's `web` on the run's search and reading models: its command lines show in the
/// TUI, and its models' tokens count toward the calling agent's job.
pub fn web(work: &Arc<Work>) -> Web {
    let (shows, charges) = (work.clone(), work.clone());
    Web::new(work.web_http.clone(), work.web.clone())
        .on_command(move |agent, line| activity(&shows, agent, "web", line))
        .on_usage(move |agent, command, usage| {
            let tokens = Tokens { input: usage.input, cached_input: usage.cached_input, output: usage.output };
            charges.charge(agent, command, &tokens);
        })
}

pub const COUNTY_TOOLS: [&str; 3] = ["web", "arcgis", "spec"];
pub const STATE_TOOLS: [&str; 3] = ["web", "arcgis", STATEWIDE_SPEC];

/// Shows the command line in the TUI's agent table.
fn activity(work: &Work, agent: &str, program: &str, line: &str) {
    let line = line.trim();
    let shown = if line.starts_with(program) { line.to_owned() } else { format!("{program} {line}") };
    work.with_agent(agent, |a| a.activity = text::short(&shown.replace('\n', " "), 200));
}
