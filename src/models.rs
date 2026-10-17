//! The models agents run on and the providers that serve them. ErisAgentAuth lists each login's
//! models (ChatGPT's through the Codex endpoint, Claude's through the Anthropic API) and builds
//! the providers that call them; this is the scraper's policy over them: the ladder of models,
//! cheapest first.

use anyhow::{Result, bail};
use erisagentauth::claude::ClaudeAccount;
use erisagentauth::openai::OpenAiAccount;
use erisharness::provider::{Provider, RateGate};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub use erisagentauth::models::{ANTHROPIC, CODEX, Model};

/// Claude model families a ladder may name, each meaning the newest listed model of it.
const CLAUDE_FAMILIES: [&str; 3] = ["haiku", "sonnet", "opus"];
/// Retries a model call makes before its attempt fails.
const RETRIES: u32 = 6;

/// One rung of the ladder: a model, who serves it, and how hard it thinks.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rung {
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
    pub context_window: Option<u64>,
}

impl Rung {
    pub fn label(&self) -> String {
        match &self.effort {
            Some(effort) => format!("{} ({effort})", self.model),
            None => self.model.clone(),
        }
    }
}

/// The newest listed Claude model of `family` (haiku, sonnet or opus).
fn newest_claude<'a>(models: &'a [Model], family: &str) -> Option<&'a Model> {
    models.iter().find(|m| m.provider == ANTHROPIC && m.cannot_run.is_none() && m.id.contains(family))
}

fn rung(model: &Model, effort: Option<&str>) -> Rung {
    Rung {
        provider: model.provider.clone(),
        model: model.id.clone(),
        effort: effort.filter(|e| model.efforts.iter().any(|m| m == e)).map(str::to_owned),
        context_window: model.context_window,
    }
}

/// The cheapest-first ladder from the signed-in logins' models. With Claude: the newest Haiku at
/// medium effort, then Sonnet at high, then ChatGPT's flagship at high, or Opus at high without
/// ChatGPT. With ChatGPT
/// alone: its most preferred small model at medium effort, then its flagship at medium and high.
pub fn default_ladder(models: &[Model]) -> Vec<Rung> {
    let codex: Vec<&Model> = models.iter().filter(|m| m.provider == CODEX).collect();
    let flagship = codex.iter().find(|m| !m.small());
    let mut ladder = Vec::new();
    if models.iter().any(|m| m.provider == ANTHROPIC) {
        ladder.extend(newest_claude(models, "haiku").map(|m| rung(m, Some("medium"))));
        ladder.extend(newest_claude(models, "sonnet").map(|m| rung(m, Some("high"))));
        match flagship {
            Some(m) => ladder.push(rung(m, Some("high"))),
            None => ladder.extend(newest_claude(models, "opus").map(|m| rung(m, Some("high")))),
        }
        return ladder;
    }
    if let Some(m) = codex.iter().find(|m| m.small()) {
        ladder.push(rung(m, Some("medium")));
    }
    if let Some(m) = flagship {
        ladder.push(rung(m, Some("medium")));
        if m.efforts.iter().any(|e| e == "high") {
            ladder.push(rung(m, Some("high")));
        }
    }
    ladder
}

/// A ladder written as `model[:effort],model[:effort],…`. `haiku`, `sonnet` and `opus` mean the
/// newest listed Claude model of that family. A listed model must be one agents can run on, and
/// take the effort named. A model the listing names is served by its
/// provider; an unlisted one by Anthropic when its id starts with `claude`, else by Codex.
pub fn parse_ladder(text: &str, models: &[Model]) -> Result<Vec<Rung>> {
    let mut ladder = Vec::new();
    for part in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (name, effort) = match part.split_once(':') {
            Some((m, e)) => (m, Some(e.to_owned())),
            None => (part, None),
        };
        let listed = if CLAUDE_FAMILIES.contains(&name) {
            match newest_claude(models, name) {
                Some(model) => Some(model),
                None => bail!("no Claude {name} model is listed; bind a Claude login (login --claude)"),
            }
        } else {
            models.iter().find(|m| m.id == name)
        };
        let provider = match listed {
            Some(model) => model.provider.clone(),
            None if name.starts_with("claude") => ANTHROPIC.into(),
            None => CODEX.into(),
        };
        if let Some(model) = listed {
            if let Some(reason) = &model.cannot_run {
                bail!("{part}: {reason}");
            }
            if let Some(effort) = &effort
                && !model.efforts.contains(effort)
            {
                bail!("{part}: {} takes the efforts {}, not {effort}", model.id, model.efforts.join(", "));
            }
        }
        ladder.push(Rung {
            provider,
            model: listed.map_or(name, |m| m.id.as_str()).to_owned(),
            effort,
            context_window: listed.and_then(|m| m.context_window),
        });
    }
    Ok(ladder)
}

/// The providers agents run on, each authorized by its ErisAgentAuth account on every request:
/// [`CODEX`] with the ChatGPT login at `codex_api`, and [`ANTHROPIC`] with the Claude login,
/// calling as Claude Code does.
pub struct Providers {
    pub codex: Arc<dyn Provider>,
    pub anthropic: Arc<dyn Provider>,
}

impl Providers {
    pub async fn new(openai: &OpenAiAccount, claude: &ClaudeAccount, codex_api: &str) -> Result<Self> {
        Ok(Self {
            codex: Arc::new(openai.codex(codex_api, RateGate::new(16, 256), RETRIES)),
            anthropic: Arc::new(claude.anthropic(RETRIES).await?),
        })
    }

    /// Each provider by the name rungs give it.
    pub fn named(&self) -> Vec<(&'static str, Arc<dyn Provider>)> {
        vec![(CODEX, self.codex.clone()), (ANTHROPIC, self.anthropic.clone())]
    }

    /// What `web` searches and reads pages with, from `listed`: with a Claude login, the newest
    /// Haiku, searching with Claude's hosted search; with ChatGPT alone, its small model,
    /// searching with OpenAI's.
    pub fn web(&self, listed: &[Model]) -> erisharness::tools::web::Models {
        erisagentauth::web_models(listed, self.codex.clone(), self.anthropic.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A listed model: `provider`'s `id` with its context window and efforts.
    fn model(provider: &str, id: &str, context_window: u64, efforts: &[&str]) -> Model {
        Model {
            provider: provider.into(),
            id: id.into(),
            name: id.into(),
            context_window: Some(context_window),
            efforts: efforts.iter().map(|e| e.to_string()).collect(),
            cannot_run: None,
        }
    }

    fn codex() -> Vec<Model> {
        vec![
            model(CODEX, "gpt-9-codex", 400000, &["low", "medium", "high"]),
            model(CODEX, "gpt-9-codex-mini", 200000, &["medium"]),
        ]
    }

    const ALL: &[&str] = &["low", "medium", "high", "xhigh", "max"];

    fn claude() -> Vec<Model> {
        let budget_only = Model {
            cannot_run: Some("claude-haiku-8 thinks only on a budget, and agents run on adaptive thinking".into()),
            ..model(ANTHROPIC, "claude-haiku-8", 200000, &[])
        };
        vec![
            model(ANTHROPIC, "claude-sonnet-9", 200000, ALL),
            model(ANTHROPIC, "claude-haiku-9", 200000, ALL),
            budget_only,
            model(ANTHROPIC, "claude-opus-9", 200000, &["low", "medium", "high"]),
        ]
    }

    fn labels(ladder: &[Rung]) -> Vec<String> {
        ladder.iter().map(|r| format!("{}/{}", r.provider, r.label())).collect()
    }

    #[test]
    fn with_chatgpt_alone_the_ladder_starts_on_its_small_model() {
        let listed = vec![
            model(CODEX, "gpt-6.1-sol", 400000, &["low", "high"]),
            model(CODEX, "gpt-6-luna", 200000, &["low", "medium"]),
        ];
        assert_eq!(default_ladder(&listed)[0].model, "gpt-6-luna");
    }

    #[test]
    fn with_chatgpt_alone_the_ladder_climbs_from_the_small_model_to_the_flagship() {
        let ladder = default_ladder(&codex());
        assert_eq!(
            labels(&ladder),
            ["codex/gpt-9-codex-mini (medium)", "codex/gpt-9-codex (medium)", "codex/gpt-9-codex (high)"]
        );
        assert_eq!(ladder[0].context_window, Some(200000));
    }

    #[test]
    fn with_claude_the_ladder_starts_on_haiku() {
        assert_eq!(
            labels(&default_ladder(&claude())),
            ["anthropic/claude-haiku-9 (medium)", "anthropic/claude-sonnet-9 (high)", "anthropic/claude-opus-9 (high)"]
        );
        let both: Vec<Model> = claude().into_iter().chain(codex()).collect();
        assert_eq!(
            labels(&default_ladder(&both)),
            ["anthropic/claude-haiku-9 (medium)", "anthropic/claude-sonnet-9 (high)", "codex/gpt-9-codex (high)"]
        );
    }

    #[test]
    fn a_written_ladder_names_models_by_id_or_claude_family() {
        let both: Vec<Model> = claude().into_iter().chain(codex()).collect();
        let ladder = parse_ladder("haiku:low, sonnet, gpt-9-codex:high, claude-new:max", &both).unwrap();
        assert_eq!(
            labels(&ladder),
            [
                "anthropic/claude-haiku-9 (low)",
                "anthropic/claude-sonnet-9",
                "codex/gpt-9-codex (high)",
                "anthropic/claude-new (max)"
            ]
        );
        assert_eq!(ladder[0].context_window, Some(200000));
        assert!(parse_ladder("opus", &codex()).unwrap_err().to_string().contains("no Claude opus"));
    }

    #[test]
    fn a_rung_takes_only_an_effort_its_model_lists() {
        let both: Vec<Model> = claude().into_iter().chain(codex()).collect();
        let error = parse_ladder("opus:max", &both).unwrap_err().to_string();
        assert!(error.contains("low, medium, high"), "{error}");
        assert!(parse_ladder("gpt-9-codex-mini:high", &both).is_err());
        let error = parse_ladder("claude-haiku-8", &both).unwrap_err().to_string();
        assert!(error.contains("adaptive"), "a model that only thinks on a budget cannot run: {error}");
    }
}
