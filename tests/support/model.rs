//! A stand-in for the model endpoints, the Codex Responses endpoint and the Anthropic Messages
//! API (with a Claude login's profile and model list): each request is answered by a brain that
//! reads it (the provider and model asked for, the briefing, the tool results so far) and
//! decides the reply.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub enum Reply {
    Says(String),
    Calls(String, Value),
    /// Never answers, like a model call still in flight.
    Hangs,
}

pub type Brain = Arc<dyn Fn(&Request) -> Reply + Send + Sync>;

/// What the brain sees of a request.
pub struct Request {
    /// `codex` or `anthropic`, by the endpoint asked.
    pub provider: &'static str,
    /// The request's `authorization` header.
    pub authorization: String,
    pub model: String,
    /// What the request asks of the model's thinking: its `thinking` type and effort, if any.
    pub thinking: Option<String>,
    pub effort: Option<String>,
    /// Whether the request offers Claude's server-side web search, as `web search` does.
    pub web_search: bool,
    pub instructions: String,
    /// The user messages, in order.
    pub user: Vec<String>,
    /// Tool results, in order.
    pub outputs: Vec<String>,
}

impl Request {
    pub fn last_output(&self) -> &str {
        self.outputs.last().map_or("", String::as_str)
    }
    pub fn briefing(&self) -> &str {
        self.user.first().map_or("", String::as_str)
    }
}

#[derive(Clone)]
struct Shared {
    brain: Brain,
    requests: Arc<Mutex<Vec<Request>>>,
}

pub struct Model {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Request>>>,
}

fn read_anthropic(body: &Value, authorization: String) -> Request {
    let text = |block: &Value| block["text"].as_str().map(str::to_owned);
    let mut user = Vec::new();
    let mut outputs = Vec::new();
    for message in body["messages"].as_array().into_iter().flatten().filter(|m| m["role"] == "user") {
        for block in message["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => user.extend(text(block)),
                Some("tool_result") => outputs.push(match &block["content"] {
                    Value::String(s) => s.clone(),
                    blocks => blocks.as_array().into_iter().flatten().filter_map(text).collect::<Vec<_>>().join(""),
                }),
                _ => {}
            }
        }
    }
    let instructions = match &body["system"] {
        Value::String(s) => s.clone(),
        blocks => blocks.as_array().into_iter().flatten().filter_map(text).collect::<Vec<_>>().join("\n"),
    };
    Request {
        provider: "anthropic",
        authorization,
        model: body["model"].as_str().unwrap_or_default().to_owned(),
        thinking: body["thinking"]["type"].as_str().map(str::to_owned),
        effort: body["output_config"]["effort"].as_str().map(str::to_owned),
        web_search: body["tools"].as_array().into_iter().flatten().any(|t| t["type"] == "web_search_20250305"),
        instructions,
        user,
        outputs,
    }
}

fn authorization(headers: &HeaderMap) -> String {
    headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned()
}

fn read(body: &Value, authorization: String) -> Request {
    let mut user = Vec::new();
    let mut outputs = Vec::new();
    for item in body["input"].as_array().into_iter().flatten() {
        if item["role"] == "user" {
            for part in item["content"].as_array().into_iter().flatten() {
                if let Some(text) = part["text"].as_str() {
                    user.push(text.to_owned());
                }
            }
        } else if item["type"] == "function_call_output" {
            outputs.push(item["output"].as_str().unwrap_or_default().to_owned());
        }
    }
    Request {
        provider: "codex",
        authorization,
        model: body["model"].as_str().unwrap_or_default().to_owned(),
        thinking: None,
        effort: body["reasoning"]["effort"].as_str().map(str::to_owned),
        web_search: false,
        instructions: body["instructions"].as_str().unwrap_or_default().to_owned(),
        user,
        outputs,
    }
}

fn sse(events: &[Value]) -> Response {
    let body: String =
        events.iter().map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap())).collect();
    ([("content-type", "text/event-stream")], body).into_response()
}

static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

async fn respond_anthropic(State(shared): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let request = read_anthropic(&serde_json::from_str(&body).unwrap(), authorization(&headers));
    let web_search = request.web_search;
    let reply = (shared.brain)(&request);
    shared.requests.lock().unwrap().push(request);
    if web_search {
        // The search, its results (titled from the URLs the brain's lines name), and the
        // brain's lines, streamed as Anthropic streams a hosted search.
        let Reply::Says(lines) = reply else { panic!("a search request is answered with text") };
        let results: Vec<Value> = lines
            .lines()
            .filter_map(|l| l.split_once(" — "))
            .map(|(url, _)| json!({"type": "web_search_result", "url": url.trim(), "title": format!("Title of {}", url.trim()), "encrypted_content": "x"}))
            .collect();
        let blocks = [
            json!({"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search", "input": {"query": "q"}}),
            json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1", "content": results}),
            json!({"type": "text", "text": lines}),
        ];
        let mut events = vec![
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 300, "cache_read_input_tokens": 0, "output_tokens": 1}}}),
        ];
        for (index, block) in blocks.into_iter().enumerate() {
            events.push(json!({"type": "content_block_start", "index": index, "content_block": block}));
            events.push(json!({"type": "content_block_stop", "index": index}));
        }
        events.extend([
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 40}}),
            json!({"type": "message_stop"}),
        ]);
        return sse(&events);
    }
    let (block, delta, stop) = match reply {
        Reply::Hangs => {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            return ().into_response();
        }
        Reply::Says(text) => (json!({"type": "text", "text": text}), None, "end_turn"),
        Reply::Calls(name, args) => {
            let id = CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (
                json!({"type": "tool_use", "id": format!("toolu_{id}"), "name": name, "input": {}}),
                Some(json!({"type": "content_block_delta", "index": 0,
                    "delta": {"type": "input_json_delta", "partial_json": args.to_string()}})),
                "tool_use",
            )
        }
    };
    let mut events = vec![
        json!({"type": "message_start", "message": {"usage": {"input_tokens": 100, "cache_read_input_tokens": 50, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": block}),
    ];
    events.extend(delta);
    events.extend([
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 20}}),
        json!({"type": "message_stop"}),
    ]);
    sse(&events)
}

/// The account a Claude login belongs to, as Anthropic's profile endpoint says.
async fn profile() -> axum::Json<Value> {
    axum::Json(json!({"account": {"uuid": "claude-account", "email": "person@example.test"},
        "organization": {"organization_type": "claude_max"}}))
}

/// A Claude model as Anthropic lists it: thinking adaptively at every effort, or, when
/// `adaptive` is false, only on a budget.
fn claude_model(id: &str, adaptive: bool) -> Value {
    let level = json!({"supported": adaptive});
    json!({"id": id, "max_input_tokens": 200000, "capabilities": {
        "effort": {"supported": adaptive, "low": level, "medium": level, "high": level, "xhigh": level, "max": level},
        "thinking": {"supported": true, "types": {"adaptive": {"supported": adaptive}, "enabled": {"supported": true}}}}})
}

/// The Claude models a login offers, newest first.
async fn claude_models() -> axum::Json<Value> {
    axum::Json(json!({"data": [
        claude_model("claude-sonnet-9", true),
        claude_model("claude-haiku-9", true),
        claude_model("claude-haiku-8", false),
        claude_model("claude-opus-9", true),
    ], "has_more": false}))
}

async fn respond(State(shared): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let request = read(&serde_json::from_str(&body).unwrap(), authorization(&headers));
    let reply = (shared.brain)(&request);
    shared.requests.lock().unwrap().push(request);
    let completed = json!({"type": "response.completed", "response": {"usage": {"input_tokens": 100, "output_tokens": 20, "input_tokens_details": {"cached_tokens": 50}}}});
    let item = match reply {
        Reply::Hangs => {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            return ().into_response();
        }
        Reply::Says(text) => {
            json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]})
        }
        Reply::Calls(name, args) => {
            let id = CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            json!({"type": "function_call", "call_id": format!("call-{id}"), "name": name, "arguments": args.to_string()})
        }
    };
    sse(&[json!({"type": "response.output_item.done", "item": item}), completed])
}

impl Model {
    pub async fn start(brain: Brain) -> Self {
        let requests: Arc<Mutex<Vec<Request>>> = Arc::default();
        let app = axum::Router::new()
            .route("/responses", axum::routing::post(respond))
            .route("/v1/messages", axum::routing::post(respond_anthropic))
            .route("/v1/models", axum::routing::get(claude_models))
            .route("/api/oauth/profile", axum::routing::get(profile))
            .with_state(Shared { brain, requests: requests.clone() });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, requests }
    }
}
