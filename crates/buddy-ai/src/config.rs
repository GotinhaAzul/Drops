//! Provider configuration.
//!
//! Capabilities are declared, never detected. The struct is intentionally
//! limited to the fields in the internal contract; hosts that need extra knobs
//! can merge them into the request themselves.

use serde::{Deserialize, Serialize};

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

/// Connection settings for an OpenAI-compatible chat completions endpoint.
///
/// `api_key_env` names an environment variable. The value is read only from
/// that variable and is never stored in this struct or logged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    #[serde(default)]
    pub vision: bool,
    #[serde(default = "default_tools")]
    pub tools: bool,
    #[serde(default = "default_max_tokens_param")]
    pub max_tokens_param: String,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            base_url: default_base_url(),
            model: String::new(),
            api_key_env: default_api_key_env(),
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
        assert!(!config.vision);
        assert!(config.tools);
        assert_eq!(config.max_tokens_param, "max_tokens");
    }

    #[test]
    fn round_trips() {
        let config = ProviderConfig {
            base_url: "http://example.test/v1".into(),
            model: "m".into(),
            api_key_env: "TEST_KEY".into(),
            vision: true,
            tools: false,
            max_tokens_param: "max_completion_tokens".into(),
        };
        let text = serde_json::to_string(&config).expect("serialize");
        let back: ProviderConfig = serde_json::from_str(&text).expect("deserialize");
        assert_eq!(back.base_url, config.base_url);
        assert_eq!(back.model, config.model);
        assert_eq!(back.api_key_env, config.api_key_env);
        assert!(back.vision);
        assert!(!back.tools);
        assert_eq!(back.max_tokens_key(), "max_completion_tokens");
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
