use serde::Deserialize;
use serde_json::Value;
/// Explicit user-authored context/capability rules. All provided selectors must match.
#[derive(Clone, Deserialize, Default)]
pub struct AnchorRule {
    #[serde(default)]
    pub class: Option<String>,
    #[serde(default)]
    pub title_contains: Option<String>,
    #[serde(default)]
    pub command_contains: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub allow_shell: bool,
}
impl AnchorRule {
    pub fn matches(&self, context: &Value) -> bool {
        let class = context["window"]["class"].as_str().unwrap_or("");
        let title = context["window"]["title"].as_str().unwrap_or("");
        let command = context["process"]["cmdline"].to_string();
        self.class
            .as_ref()
            .is_none_or(|v| v.eq_ignore_ascii_case(class))
            && self
                .title_contains
                .as_ref()
                .is_none_or(|v| title.contains(v))
            && self
                .command_contains
                .as_ref()
                .is_none_or(|v| command.contains(v))
            && self.kind.as_ref().is_none_or(|v| context["kind"] == *v)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_selectors_must_match() {
        let r = AnchorRule {
            class: Some("kitty".into()),
            title_contains: Some("project".into()),
            ..Default::default()
        };
        assert!(r.matches(&serde_json::json!({"window":{"class":"kitty","title":"project"}})));
        assert!(
            !r.matches(&serde_json::json!({"window":{"class":"browser","title":"kitty project"}}))
        );
    }
}
