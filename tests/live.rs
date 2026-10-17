//! Calls to the real search and reading models, run by hand on the logins in a state directory:
//! `ERISGISSCRAPER_LIVE_STATE=<state dir> cargo test --test live -- --ignored --nocapture`.
//! One test uses the bound Claude login (Claude's hosted search, Haiku reading), the other the
//! ChatGPT login alone (OpenAI's hosted search on the Codex endpoint, its small model reading).

use erisagentauth::{Accounts, App};
use erisgisscraper::models::{Model, Providers};
use erisharness::tools::web::{Http, Limits, Web, shown};
use std::path::PathBuf;

/// The logins kept in the state directory, at the providers' own servers.
fn logins() -> Accounts {
    let state = PathBuf::from(std::env::var("ERISGISSCRAPER_LIVE_STATE").expect("ERISGISSCRAPER_LIVE_STATE"));
    Accounts::load(&state.join("accounts"), App { name: "live", originator: "live" }).unwrap()
}

/// One search and one page read through `web` on `listed`'s models.
async fn search_and_read(logins: &Accounts, listed: &[Model]) {
    let providers = Providers::new(&logins.openai, &logins.claude, erisagentauth::openai::CODEX_API).await.unwrap();
    let models = providers.web(listed);
    let searcher = models.searcher.as_ref().expect("a model to search with");
    println!("searching and reading on {} ({:?})", searcher.model, searcher.effort);
    let web = Web::new(Http::new(Limits::default(), erisgis::USER_AGENT).unwrap(), models);
    let (found, usage) = web.search("Wyandot County Ohio zoning GIS ArcGIS REST").await.unwrap();
    println!("search: {usage:?}");
    for f in &found {
        println!("- {} | {} | {}", f.title, f.url, f.about);
    }
    assert!(!found.is_empty());
    let url = "https://landgauge.com/ohio/wyandot-county";
    let fetched = web.http().get(url, 8 << 20).await.unwrap();
    let page = shown(&fetched.text(), &fetched.content_type, &fetched.url);
    let (answer, usage) = web.read(url, &page, "which links lead to parcel data or GIS services?").await.unwrap();
    println!("read ({} characters of page): {usage:?}\n{answer}", page.chars().count());
    assert!(!answer.is_empty());
}

#[tokio::test]
#[ignore]
async fn a_real_search_and_read_on_claude() {
    let logins = logins();
    let listed = logins.claude.models().await.unwrap();
    search_and_read(&logins, &listed).await;
}

#[tokio::test]
#[ignore]
async fn a_real_search_and_read_on_chatgpt() {
    let logins = logins();
    let listed = logins.openai.models(erisagentauth::openai::CODEX_API).await.unwrap();
    search_and_read(&logins, &listed).await;
}
