//! What `arcgis` shows agents: windows of long answers, and HTML descriptions as plain text.

pub use erisharness::tools::web::short;
use erisharness::tools::web::{readable, window_of};

/// Characters of an answer shown per call.
pub const WINDOW: usize = 16_000;

/// The window of `text` starting at `offset` characters, with where it sits.
pub fn window(text: &str, offset: usize) -> String {
    window_of(text, offset, WINDOW)
}

pub fn strip_html(text: &str) -> String {
    readable(text, "https://www.arcgis.com/").trim().replace('\n', " ")
}
