//! Tool registry and built-in tool definitions.

mod discovery;
mod external;
mod files;
mod office;
mod process;
mod shell;
mod web;

use std::path::PathBuf;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::context_files::Skill;
use crate::{ContextTool, ExternalTool};

const MAX_TOOL_OUTPUT_BYTES: usize = 60_000;

/// Tool risk classification from the product spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Risk {
    ReadOnly,
    Mutating,
    Dangerous,
}

impl Risk {
    /// Unknown values are dangerous, never read-only.
    pub(crate) fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "read" | "readonly" | "read-only" | "safe" => Self::ReadOnly,
            "mutating" | "mutation" | "write" | "writes" => Self::Mutating,
            _ => Self::Dangerous,
        }
    }

    pub(crate) fn requires_approval(self) -> bool {
        !matches!(self, Self::ReadOnly)
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::Mutating => "mutating",
            Self::Dangerous => "dangerous",
        }
    }
}

pub(crate) struct ToolOutcome {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutcome {
    pub(crate) fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub(crate) fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

pub(crate) struct ToolContext {
    pub root: PathBuf,
    pub allow_shell: bool,
    pub skills: Vec<Skill>,
}

#[derive(Clone)]
pub(crate) enum Tool {
    Read,
    ListFiles,
    SearchFiles,
    Append,
    WebSearch,
    WebFetch,
    DocumentRead,
    DocumentEdit,
    Write,
    Edit,
    Bash,
    Skill,
    External(ExternalTool),
    Context(ContextTool),
}

impl Tool {
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Read => "read",
            Self::ListFiles => "list_files",
            Self::SearchFiles => "search_files",
            Self::Append => "append",
            Self::WebSearch => "web_search",
            Self::WebFetch => "web_fetch",
            Self::DocumentRead => "document_read",
            Self::DocumentEdit => "document_edit",
            Self::Write => "write",
            Self::Edit => "edit",
            Self::Bash => "bash",
            Self::Skill => "skill",
            Self::External(tool) => &tool.name,
            Self::Context(tool) => &tool.name,
        }
    }

    pub(crate) fn risk(&self) -> Risk {
        match self {
            Self::Read | Self::ListFiles | Self::SearchFiles | Self::DocumentRead | Self::Skill => {
                Risk::ReadOnly
            }
            Self::Write | Self::Edit | Self::Append | Self::DocumentEdit => Risk::Mutating,
            Self::Bash | Self::WebSearch | Self::WebFetch => Risk::Dangerous,
            Self::External(tool) => Risk::parse(&tool.risk),
            Self::Context(tool) => Risk::parse(&tool.risk),
        }
    }

    pub(crate) fn definition(&self) -> Value {
        match self {
            Self::ListFiles | Self::SearchFiles => {
                discovery::definition(matches!(self, Self::SearchFiles))
            }
            Self::Append => json!({
                "type": "function", "function": {
                    "name": "append",
                    "description": "Append exact text to a workspace file, creating it and parent directories if missing. Does not add a newline automatically. Requires user approval.",
                    "parameters": {"type":"object", "properties": {
                        "path":{"type":"string"}, "content":{"type":"string"}
                    }, "required":["path","content"]}
                }
            }),
            Self::DocumentRead | Self::DocumentEdit => {
                office::definition(matches!(self, Self::DocumentEdit))
            }
            Self::WebSearch | Self::WebFetch => web::definition(matches!(self, Self::WebSearch)),
            Self::Read => json!({
                "type": "function",
                "function": {
                    "name": "read",
                    "description": "Read a UTF-8 text file inside the project directory. Output is truncated for large files; use offset and limit to continue.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Path relative to the project root (absolute paths must stay inside it)." },
                            "offset": { "type": "integer", "minimum": 1, "description": "1-based line number to start at (default 1)." },
                            "limit": { "type": "integer", "minimum": 1, "maximum": 5000, "description": "Maximum lines to return (default 2000)." }
                        },
                        "required": ["path"]
                    }
                }
            }),
            Self::Write => json!({
                "type": "function",
                "function": {
                    "name": "write",
                    "description": "Create or overwrite a file inside the project directory. Parent directories are created automatically. Requires user approval.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Path relative to the project root." },
                            "content": { "type": "string", "description": "Full content to write." }
                        },
                        "required": ["path", "content"]
                    }
                }
            }),
            Self::Edit => json!({
                "type": "function",
                "function": {
                    "name": "edit",
                    "description": "Edit a workspace text file using a single exact replacement or a batch of non-overlapping edits against the original text. Batch edits are all validated before writing; a failed match leaves the file unchanged. Requires user approval.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Path relative to the project root." },
                            "old_string": { "type": "string", "description": "Exact text to replace. Include enough context to be unique." },
                            "new_string": { "type": "string", "description": "Replacement text." },
                            "replace_all": { "type": "boolean", "description": "Replace every occurrence for a single replacement; cannot be combined with edits." },
                            "expected_content": {"type":"string", "description":"Optional full previously read file content. Refuse the edit if the current file differs."},
                            "edits": {"type":"array", "minItems":1, "maxItems":100, "description":"Batch replacements against the original file; use instead of old_string/new_string.", "items":{
                                "type":"object", "properties": {"old_string":{"type":"string", "minLength":1}, "new_string":{"type":"string"}}, "required":["old_string","new_string"]
                            }}
                        },
                        "required": ["path"],
                        "oneOf": [{"required":["old_string","new_string"], "not":{"required":["edits"]}}, {"required":["edits"], "not":{"anyOf":[{"required":["old_string"]},{"required":["new_string"]}]}}]
                    }
                }
            }),
            Self::Bash => json!({
                "type": "function",
                "function": {
                    "name": "bash",
                    "description": "Run a bash command in the project directory. The user must approve every invocation. Returns combined output and the exit code.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "command": { "type": "string", "description": "Command line to run with bash -c." },
                            "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 600, "description": "Timeout in seconds (default 120)." }
                        },
                        "required": ["command"]
                    }
                }
            }),
            Self::Skill => json!({
                "type": "function",
                "function": {
                    "name": "skill",
                    "description": "Load the full instructions for an available skill by name. Use this before following a skill listed in the system prompt.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "description": "Skill name exactly as listed." }
                        },
                        "required": ["name"]
                    }
                }
            }),
            Self::Context(tool) => {
                let parameters = if tool.parameters.is_object() {
                    tool.parameters.clone()
                } else {
                    json!({ "type": "object", "properties": {}, "additionalProperties": true })
                };
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": parameters
                    }
                })
            }
            Self::External(tool) => {
                let parameters = if tool.parameters.is_object() {
                    tool.parameters.clone()
                } else {
                    json!({ "type": "object", "properties": {}, "additionalProperties": true })
                };
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": parameters
                    }
                })
            }
        }
    }
}

pub(crate) struct ToolRegistry {
    ctx: ToolContext,
    tools: Vec<Tool>,
}

impl ToolRegistry {
    pub(crate) fn new(
        root: PathBuf,
        allow_shell: bool,
        context: Vec<ContextTool>,
        external: Vec<ExternalTool>,
        skills: Vec<Skill>,
    ) -> Self {
        let mut tools = vec![
            Tool::Read,
            Tool::ListFiles,
            Tool::SearchFiles,
            Tool::Write,
            Tool::Edit,
            Tool::Append,
            Tool::WebSearch,
            Tool::WebFetch,
            Tool::DocumentRead,
            Tool::DocumentEdit,
        ];
        if allow_shell {
            tools.push(Tool::Bash);
        }
        if !skills.is_empty() {
            tools.push(Tool::Skill);
        }
        // Desktop-provided tools sit between built-ins and user external tools:
        // they cannot shadow core behavior, and a broken external command
        // cannot shadow the anchor tool the daemon just advertised.
        for tool in context {
            if tool.name.trim().is_empty() {
                continue;
            }
            if tools.iter().any(|existing| existing.name() == tool.name) {
                continue;
            }
            tools.push(Tool::Context(tool));
        }
        for tool in external {
            if tool.name.trim().is_empty() {
                continue;
            }
            // Built-ins always win, and the first tool with a given name wins
            // over later duplicates.
            if tools.iter().any(|existing| existing.name() == tool.name) {
                continue;
            }
            tools.push(Tool::External(tool));
        }
        Self {
            ctx: ToolContext {
                root,
                allow_shell,
                skills,
            },
            tools,
        }
    }

    pub(crate) fn definitions(&self) -> Vec<Value> {
        self.tools.iter().map(Tool::definition).collect()
    }

    pub(crate) fn names(&self) -> Vec<&str> {
        self.tools.iter().map(Tool::name).collect()
    }

    pub(crate) fn find(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|tool| tool.name() == name)
    }

    pub(crate) async fn execute(
        &self,
        tool: &Tool,
        arguments: &Value,
        cancel: &CancellationToken,
    ) -> ToolOutcome {
        let outcome = match tool {
            Tool::Read => files::read(&self.ctx, arguments),
            Tool::ListFiles => discovery::execute(&self.ctx, arguments, false, cancel),
            Tool::SearchFiles => discovery::execute(&self.ctx, arguments, true, cancel),
            Tool::Append => files::append(&self.ctx, arguments),
            Tool::WebSearch => web::execute(arguments, true, cancel).await,
            Tool::WebFetch => web::execute(arguments, false, cancel).await,
            Tool::DocumentRead => office::execute(&self.ctx, arguments, false, cancel).await,
            Tool::DocumentEdit => office::execute(&self.ctx, arguments, true, cancel).await,
            Tool::Write => files::write(&self.ctx, arguments),
            Tool::Edit => files::edit(&self.ctx, arguments),
            Tool::Bash => shell::bash(&self.ctx, arguments, cancel).await,
            Tool::Skill => skill(&self.ctx, arguments),
            Tool::Context(context) => match context.handler.call(arguments, cancel).await {
                Ok(content) => ToolOutcome::ok(content),
                Err(err) => ToolOutcome::error(format!("{err:#}")),
            },
            Tool::External(external) => {
                external::execute(&self.ctx, external, arguments, cancel).await
            }
        };
        ToolOutcome {
            content: truncate_output(outcome.content),
            is_error: outcome.is_error,
        }
    }
}

fn skill(ctx: &ToolContext, arguments: &Value) -> ToolOutcome {
    let name = match string_arg(arguments, "name") {
        Ok(name) => name,
        Err(outcome) => return outcome,
    };
    match ctx.skills.iter().find(|skill| skill.name == name) {
        Some(skill) => {
            let mut content = skill.body.clone();
            if let Some(location) = &skill.location {
                content.push_str(&format!(
                    "\n\n[Skill files are under `{location}` inside the project root.]"
                ));
            }
            if content.trim().is_empty() {
                ToolOutcome::ok(format!("Skill `{name}` has no additional instructions."))
            } else {
                ToolOutcome::ok(content)
            }
        }
        None => {
            let available = if ctx.skills.is_empty() {
                "none".to_string()
            } else {
                ctx.skills
                    .iter()
                    .map(|skill| skill.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            ToolOutcome::error(format!(
                "Unknown skill `{name}`. Available skills: {available}."
            ))
        }
    }
}

pub(crate) fn string_arg<'a>(arguments: &'a Value, key: &str) -> Result<&'a str, ToolOutcome> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolOutcome::error(format!("Missing required string argument `{key}`.")))
}

pub(crate) fn optional_u64(arguments: &Value, key: &str) -> Option<u64> {
    arguments.get(key).and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))
    })
}

pub(crate) fn truncate_output(content: String) -> String {
    if content.len() <= MAX_TOOL_OUTPUT_BYTES {
        return content;
    }
    let mut end = MAX_TOOL_OUTPUT_BYTES;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    let mut content = content;
    content.truncate(end);
    content.push_str("\n...[output truncated]");
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risk_parsing_is_fail_safe() {
        assert_eq!(Risk::parse("read"), Risk::ReadOnly);
        assert_eq!(Risk::parse("read-only"), Risk::ReadOnly);
        assert_eq!(Risk::parse("READ_ONLY"), Risk::ReadOnly);
        assert_eq!(Risk::parse("mutating"), Risk::Mutating);
        assert_eq!(Risk::parse("write"), Risk::Mutating);
        assert_eq!(Risk::parse("dangerous"), Risk::Dangerous);
        assert_eq!(Risk::parse("something-else"), Risk::Dangerous);
        assert_eq!(Risk::parse(""), Risk::Dangerous);
    }

    #[test]
    fn bash_is_absent_without_allow_shell() {
        let without = ToolRegistry::new(
            PathBuf::from("/tmp"),
            false,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert!(without.find("bash").is_none());
        assert!(!without.names().contains(&"bash"));

        let with = ToolRegistry::new(
            PathBuf::from("/tmp"),
            true,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert!(with.find("bash").is_some());
        assert_eq!(with.find("bash").unwrap().risk(), Risk::Dangerous);
    }

    #[test]
    fn builtins_win_over_external_name_collisions() {
        let external = ExternalTool {
            name: "read".into(),
            description: "shadow".into(),
            command: vec!["/bin/true".into()],
            parameters: json!({"type":"object","properties":{}}),
            risk: "read-only".into(),
        };
        let registry = ToolRegistry::new(
            PathBuf::from("/tmp"),
            false,
            Vec::new(),
            vec![external],
            Vec::new(),
        );
        let read = registry.find("read").unwrap();
        assert!(matches!(read, Tool::Read));
        assert_eq!(
            registry
                .names()
                .iter()
                .filter(|name| **name == "read")
                .count(),
            1
        );
    }

    struct EchoHandler;

    impl crate::ContextToolHandler for EchoHandler {
        fn call<'a>(
            &'a self,
            arguments: &'a Value,
            _cancel: &'a CancellationToken,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send + 'a>>
        {
            Box::pin(async move { Ok(format!("echo: {arguments}")) })
        }
    }

    fn context_tool(name: &str, risk: &str) -> ContextTool {
        ContextTool::new(
            name,
            "desktop context",
            json!({"type":"object","properties":{}}),
            risk,
            std::sync::Arc::new(EchoHandler),
        )
    }

    #[test]
    fn context_tools_are_registered_after_builtins_and_default_to_approval() {
        let registry = ToolRegistry::new(
            PathBuf::from("/tmp"),
            false,
            vec![
                context_tool("window_content", "read-only"),
                context_tool("read", "read-only"),
            ],
            Vec::new(),
            Vec::new(),
        );

        let window = registry.find("window_content").expect("context tool");
        assert_eq!(window.risk(), Risk::ReadOnly);
        assert!(matches!(registry.find("read"), Some(Tool::Read)));
        assert_eq!(
            registry
                .names()
                .iter()
                .filter(|name| **name == "read")
                .count(),
            1
        );

        let unknown_risk = ToolRegistry::new(
            PathBuf::from("/tmp"),
            false,
            vec![context_tool("opaque", "surprising")],
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(unknown_risk.find("opaque").unwrap().risk(), Risk::Dangerous);
    }

    #[tokio::test]
    async fn context_tool_handler_result_becomes_the_tool_output() {
        let registry = ToolRegistry::new(
            PathBuf::from("/tmp"),
            false,
            vec![context_tool("window_content", "read-only")],
            Vec::new(),
            Vec::new(),
        );
        let tool = registry.find("window_content").expect("context tool");
        let outcome = registry
            .execute(tool, &json!({"action": "read"}), &CancellationToken::new())
            .await;
        assert!(!outcome.is_error);
        assert_eq!(outcome.content, "echo: {\"action\":\"read\"}");
    }

    #[test]
    fn definitions_are_openai_tool_objects() {
        let registry = ToolRegistry::new(
            PathBuf::from("/tmp"),
            true,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let definitions = registry.definitions();
        let names: Vec<_> = definitions
            .iter()
            .map(|definition| definition["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "read",
                "list_files",
                "search_files",
                "write",
                "edit",
                "append",
                "web_search",
                "web_fetch",
                "document_read",
                "document_edit",
                "bash"
            ]
        );
        for definition in definitions {
            assert_eq!(definition["type"], "function");
            assert!(definition["function"]["parameters"].is_object());
        }
    }

    #[test]
    fn coworker_tools_keep_reads_automatic_and_append_approved() {
        let registry = ToolRegistry::new(PathBuf::from("/tmp"), false, vec![], vec![], vec![]);
        for name in ["list_files", "search_files", "document_read"] {
            assert!(!registry.find(name).unwrap().risk().requires_approval());
        }
        assert!(registry.find("append").unwrap().risk().requires_approval());
        assert!(registry
            .find("document_edit")
            .unwrap()
            .risk()
            .requires_approval());
        for name in ["web_search", "web_fetch"] {
            assert!(registry.find(name).unwrap().risk().requires_approval());
        }
    }

    #[test]
    fn output_truncation_respects_utf8_boundaries() {
        let content = "é".repeat(MAX_TOOL_OUTPUT_BYTES);
        let truncated = truncate_output(content);
        assert!(truncated.ends_with("[output truncated]"));
        assert!(truncated.is_char_boundary(truncated.len()));
    }
}
