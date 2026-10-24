//! Provider configuration.
//!
//! Capabilities are declared, never detected. The struct is intentionally
//! limited to the fields in the internal contract; hosts that need extra knobs
//! can merge them into the request themselves.

use std::fmt;

use serde::{Deserialize, Serialize};

/// OpenAI-compatible gateway for an Opencode Go subscription.
pub const OPENCODE_GO_BASE_URL: &str = "https://opencode.ai/zen/go/v1";
/// Environment variable consulted when no Opencode Go key is stored.
pub const OPENCODE_GO_API_KEY_ENV: &str = "OPENCODE_API_KEY";
/// Routing/cache hint required by the Opencode Go gateway.
pub const OPENCODE_GO_SESSION_HEADER: &str = "x-opencode-session";

fn default_base_url() -> String {
    "http://localhost:11434/v1".to_string()
}

fn default_api_key_env() -> String {
    String::new()
}

fn default_tools() -> bool {
    true
}

fn default_max_tokens_param() -> String {
    "max_tokens".to_string()
}

/// How the connection was configured.
///
/// `Custom` keeps the original bring-your-own OpenAI-compatible endpoint.
/// `OpencodeGo` uses the Opencode Go gateway, a stored API key, and its
/// published model list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    #[default]
    Custom,
    #[serde(alias = "opencode-go")]
    OpencodeGo,
}

/// Connection settings for an OpenAI-compatible chat completions endpoint.
///
/// `api_key_env` names an environment variable. The value is read only from
/// that variable and is never stored in this struct or logged. A non-empty
/// [`ProviderConfig::api_key`] takes precedence over the environment variable;
/// the daemon stores it only for the Opencode Go provider, in a mode-0600
/// config file.
#[derive(Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default)]
    pub kind: ProviderKind,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Stable per-conversation id for Opencode Go routing and prompt caching.
    /// Set at run time by the daemon; never persisted by the daemon.
    #[serde(default, skip_serializing)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub vision: bool,
    #[serde(default = "default_tools")]
    pub tools: bool,
    #[serde(default = "default_max_tokens_param")]
    pub max_tokens_param: String,
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("kind", &self.kind)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key_env", &self.api_key_env)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("session_id", &self.session_id)
            .field("vision", &self.vision)
            .field("tools", &self.tools)
            .field("max_tokens_param", &self.max_tokens_param)
            .finish()
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            kind: ProviderKind::Custom,
            base_url: default_base_url(),
            model: String::new(),
            api_key_env: default_api_key_env(),
            api_key: None,
            session_id: None,
            vision: false,
            tools: default_tools(),
            max_tokens_param: default_max_tokens_param(),
        }
    }
}

impl ProviderConfig {
    /// Key to use for an output-token limit on this endpoint.
    ///
    /// The provider contract has no token-limit value, so [`crate::complete`]
    /// never sends one. Callers that build requests themselves can use this
    /// name (for example `max_tokens` or `max_completion_tokens`).
    pub fn max_tokens_key(&self) -> &str {
        if self.max_tokens_param.trim().is_empty() {
            "max_tokens"
        } else {
            self.max_tokens_param.as_str()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_serde_compatible() {
        let config: ProviderConfig =
            serde_json::from_str(r#"{"base_url":"http://localhost:11434/v1","model":"qwen"}"#)
                .expect("deserialize");
        assert_eq!(config.base_url, "http://localhost:11434/v1");
        assert_eq!(config.model, "qwen");
        assert_eq!(config.api_key_env, "");
        assert_eq!(config.kind, ProviderKind::Custom);
        assert!(config.api_key.is_none());
        assert!(!config.vision);
        assert!(config.tools);
        assert_eq!(config.max_tokens_param, "max_tokens");
    }

    #[test]
    fn round_trips() {
        let config = ProviderConfig {
            kind: ProviderKind::OpencodeGo,
            base_url: OPENCODE_GO_BASE_URL.into(),
            model: "m".into(),
            api_key_env: "TEST_KEY".into(),
            api_key: Some("secret".into()),
            session_id: Some("conversation-id".into()),
            vision: true,
            tools: false,
            max_tokens_param: "max_completion_tokens".into(),
        };
        let text = serde_json::to_string(&config).expect("serialize");
        let back: ProviderConfig = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back.kind, ProviderKind::OpencodeGo);
        assert_eq!(back.base_url, config.base_url);
        assert_eq!(back.model, config.model);
        assert_eq!(back.api_key_env, config.api_key_env);
        assert_eq!(back.api_key.as_deref(), Some("secret"));
        assert!(back.session_id.is_none(), "session ids are not persisted");
        assert!(back.vision);
        assert!(!back.tools);
        assert_eq!(back.max_tokens_key(), "max_completion_tokens");
    }

    #[test]
    fn debug_redacts_the_stored_key() {
        let config = ProviderConfig {
            api_key: Some("super-secret-key".into()),
            ..ProviderConfig::default()
        };
        let debug = format!("{config:?}");
        assert!(!debug.contains("super-secret-key"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn kind_accepts_both_serde_spellings() {
        let snake: ProviderConfig =
            serde_json::from_str(r#"{"kind":"opencode_go","model":"m"}"#).expect("snake_case");
        let kebab: ProviderConfig =
            serde_json::from_str(r#"{"kind":"opencode-go","model":"m"}"#).expect("kebab-case");
        assert_eq!(snake.kind, ProviderKind::OpencodeGo);
        assert_eq!(kebab.kind, ProviderKind::OpencodeGo);
    }

    #[test]
    fn empty_max_tokens_param_falls_back() {
        let config = ProviderConfig {
            max_tokens_param: "  ".into(),
            ..ProviderConfig::default()
        };
        assert_eq!(config.max_tokens_key(), "max_tokens");
    }
}
