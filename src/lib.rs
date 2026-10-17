//! ErisHarness agents that find, write and validate an ErisGIS adapter for every US county, one
//! dataset at a time: the scheduler and its model ladder, the journal a run resumes from, the
//! agents' command-line tools (ErisHarness's `web` among them), the terminal view and the field
//! notes. The specs, registry,
//! runtime and validator they work through are the `erisgis` crate's.

pub mod journal;
pub mod models;
pub mod notes;
pub mod prompts;
pub mod scheduler;
pub mod tools;
pub mod tui;
pub mod view;
pub mod work;
