//! `arcgis`: search ArcGIS Hub and ArcGIS Online for layers, and inspect ArcGIS REST services.

use super::text::{short, strip_html, window};
use crate::journal::Job;
use crate::work::Work;
use clap::{CommandFactory, Parser, Subcommand};
use erisgis::geom::Bbox;
use erisharness::tools::cli;
use erisharness::tools::{Tool, ToolContext, ToolOutput};
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "arcgis", disable_version_flag = true, about = "Find ArcGIS layers and inspect ArcGIS REST services.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Search ArcGIS Hub and ArcGIS Online for public feature services, map services and
    /// open-data datasets within your county's box (your state's, in a statewide search).
    /// Prints titles, types, owners and service URLs.
    #[command(after_help = "Example: arcgis search tax parcels")]
    Search {
        #[arg(required = true)]
        query: Vec<String>,
        /// Search within this state's box instead, by postal abbreviation.
        #[arg(long)]
        state: Option<String>,
    },
    /// Inspect an ArcGIS REST endpoint: a services directory (…/arcgis/rest/services or a
    /// folder) lists folders and services, a FeatureServer or MapServer its layers, and a layer
    /// (…/MapServer/3) its fields, maxRecordCount, paging support, query formats and feature
    /// count.
    #[command(after_help = "Example: arcgis probe https://gis.example.gov/arcgis/rest/services --grep parcel")]
    Probe {
        url: String,
        /// Show only lines matching this case-insensitive regular expression.
        #[arg(long)]
        grep: Option<String>,
    },
}

pub struct Arcgis(pub Arc<Work>);

fn union(boxes: impl Iterator<Item = Bbox>) -> Option<Bbox> {
    boxes.reduce(|a, b| {
        Bbox([a.west().min(b.west()), a.south().min(b.south()), a.east().max(b.east()), a.north().max(b.north())])
    })
}

impl Arcgis {
    pub async fn run(&self, agent: &str, line: &str) -> ToolOutput {
        super::activity(&self.0, agent, "arcgis", line);
        let cli: Cli = match cli::parse("arcgis", line) {
            Ok(cli) => cli,
            Err(output) => return output,
        };
        match cli.command {
            Command::Search { query, state } => {
                let job = self.0.with_agent(agent, |a| a.job.clone());
                let bbox = match (state, job) {
                    (Some(state), _) => {
                        let state = state.to_uppercase();
                        match union(self.0.counties.in_state(&state).map(|c| c.bbox)) {
                            Some(bbox) => Some(bbox),
                            None => return ToolOutput::error(format!("no state {state}")),
                        }
                    }
                    (None, Some(Job::County(fips) | Job::Jurisdictions(fips))) => {
                        self.0.counties.get(&fips).map(|c| c.bbox)
                    }
                    (None, Some(Job::State(state))) => union(self.0.counties.in_state(&state).map(|c| c.bbox)),
                    (None, None) => None,
                };
                ToolOutput::text(self.search(&query.join(" "), bbox).await)
            }
            Command::Probe { url, grep } => match probe(&self.0, &url).await {
                Ok(text) => match grep {
                    Some(pattern) => match cli::grep(&text, &pattern, 0) {
                        Ok(text) => ToolOutput::text(window(&text, 0)),
                        Err(error) => ToolOutput::error(error),
                    },
                    None => ToolOutput::text(window(&text, 0)),
                },
                Err(error) => ToolOutput::error(format!("{error:#}")),
            },
        }
    }

    async fn search(&self, query: &str, bbox: Option<Bbox>) -> String {
        let corners = bbox
            .map(|b| format!("{:.4},{:.4},{:.4},{:.4}", b.west(), b.south(), b.east(), b.north()))
            .unwrap_or_default();
        let mut out = Vec::new();
        let mut hub = url::Url::parse("https://hub.arcgis.com/api/search/v1/collections/all/items").expect("a URL");
        hub.query_pairs_mut().append_pair("q", query).append_pair("limit", "15");
        if !corners.is_empty() {
            hub.query_pairs_mut().append_pair("bbox", &corners);
        }
        match self.0.runtime.http.get_json(hub.as_str(), 8 << 20).await {
            Ok(body) => {
                out.push("ArcGIS Hub:".to_owned());
                for f in body["features"].as_array().into_iter().flatten() {
                    let p = &f["properties"];
                    out.push(format!(
                        "- {} [{}] by {}\n  {}\n  {}",
                        p["title"].as_str().unwrap_or("?"),
                        p["type"].as_str().unwrap_or("?"),
                        p["owner"].as_str().or(p["source"].as_str()).unwrap_or("?"),
                        p["url"].as_str().unwrap_or("(no service URL)"),
                        short(&strip_html(p["snippet"].as_str().unwrap_or_default()), 200),
                    ));
                }
            }
            Err(error) => out.push(format!("ArcGIS Hub failed: {error:#}")),
        }
        let mut online = url::Url::parse("https://www.arcgis.com/sharing/rest/search").expect("a URL");
        online
            .query_pairs_mut()
            .append_pair("q", &format!("{query} (type:\"Feature Service\" OR type:\"Map Service\")"))
            .append_pair("num", "15")
            .append_pair("f", "json");
        if !corners.is_empty() {
            online.query_pairs_mut().append_pair("bbox", &corners);
        }
        match self.0.runtime.http.get_json(online.as_str(), 8 << 20).await {
            Ok(body) => {
                out.push("ArcGIS Online:".to_owned());
                for r in body["results"].as_array().into_iter().flatten() {
                    out.push(format!(
                        "- {} [{}] by {}\n  {}\n  {}",
                        r["title"].as_str().unwrap_or("?"),
                        r["type"].as_str().unwrap_or("?"),
                        r["owner"].as_str().unwrap_or("?"),
                        r["url"].as_str().unwrap_or("(no service URL)"),
                        short(&strip_html(r["snippet"].as_str().unwrap_or_default()), 200),
                    ));
                }
            }
            Err(error) => out.push(format!("ArcGIS Online failed: {error:#}")),
        }
        out.join("\n")
    }
}

/// What an ArcGIS REST endpoint holds, briefly: a directory's folders and services, a
/// service's layers, or a layer's fields, record limit, paging and formats.
async fn probe(work: &Work, url: &str) -> anyhow::Result<String> {
    let base = url.split('?').next().unwrap_or(url).trim_end_matches('/');
    let body = work.runtime.http.get_json(&format!("{base}?f=json"), 8 << 20).await?;
    if let Some(error) = body.get("error") {
        anyhow::bail!("the server refused: {}", error["message"].as_str().unwrap_or("(no message)"));
    }
    let mut out = Vec::new();
    if body.get("fields").is_some() || body["type"] == "Feature Layer" {
        out.push(format!(
            "Layer: {} ({})",
            body["name"].as_str().unwrap_or("?"),
            body["geometryType"].as_str().unwrap_or("no geometry")
        ));
        out.push(format!("maxRecordCount: {}", body["maxRecordCount"]));
        out.push(format!(
            "supportsPagination: {}",
            body["advancedQueryCapabilities"]["supportsPagination"]
                .as_bool()
                .map_or("unknown".into(), |b| b.to_string())
        ));
        out.push(format!("supportedQueryFormats: {}", body["supportedQueryFormats"].as_str().unwrap_or("?")));
        out.push(format!("spatial reference: {}", body["extent"]["spatialReference"]));
        let count = work
            .runtime
            .http
            .get_json(&format!("{base}/query?where=1%3D1&returnCountOnly=true&f=json"), 1 << 20)
            .await
            .ok()
            .and_then(|v| v["count"].as_u64());
        out.push(format!("features: {}", count.map_or("unknown".into(), |c| c.to_string())));
        out.push("fields:".into());
        for field in body["fields"].as_array().into_iter().flatten().take(120) {
            out.push(format!(
                "  {} ({}) {}",
                field["name"].as_str().unwrap_or("?"),
                field["type"].as_str().unwrap_or("?").trim_start_matches("esriFieldType"),
                field["alias"].as_str().filter(|a| Some(*a) != field["name"].as_str()).unwrap_or_default()
            ));
        }
    } else if body.get("layers").is_some() || body.get("services").is_some() || body.get("folders").is_some() {
        if let Some(description) = body["serviceDescription"].as_str().filter(|d| !d.is_empty()) {
            out.push(format!("Service: {}", short(&strip_html(description), 300)));
        }
        for folder in body["folders"].as_array().into_iter().flatten() {
            out.push(format!("folder: {base}/{}", folder.as_str().unwrap_or("?")));
        }
        for service in body["services"].as_array().into_iter().flatten() {
            let name = service["name"].as_str().unwrap_or("?");
            let leaf = name.rsplit('/').next().unwrap_or(name);
            out.push(format!("service: {base}/{leaf}/{} ", service["type"].as_str().unwrap_or("?")));
        }
        for layer in body["layers"].as_array().into_iter().flatten() {
            out.push(format!(
                "layer: {base}/{} {} ({})",
                layer["id"],
                layer["name"].as_str().unwrap_or("?"),
                layer["geometryType"].as_str().unwrap_or("?")
            ));
        }
        if let Some(count) = body["maxRecordCount"].as_u64() {
            out.push(format!("maxRecordCount: {count}"));
        }
    } else {
        out.push(short(&body.to_string(), 3000));
    }
    Ok(out.join("\n"))
}

impl Tool for Arcgis {
    fn name(&self) -> &str {
        "arcgis"
    }
    fn description(&self) -> String {
        format!("Runs an `arcgis` command, given as a command line.\n\n{}", Cli::command().render_long_help())
    }
    fn parameters(&self) -> Value {
        cli::parameters("probe https://gis.example.gov/arcgis/rest/services/Parcels/MapServer/0")
    }
    fn snippet(&self) -> &str {
        "arcgis: search ArcGIS Hub and Online for layers, and inspect ArcGIS REST services"
    }
    fn call<'a>(&'a self, context: &'a ToolContext, args: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            match cli::line(&args, "arcgis", "search parcels") {
                Ok(line) => cli::until_stopped(context, self.run(&context.agent, &line)).await,
                Err(output) => output,
            }
        })
    }
}
