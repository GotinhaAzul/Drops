//! Tool registry and built-in tool definitions.

mod discovery;
mod external;
mod files;
mod office;
mod overview;
mod process;
mod shell;
mod web;

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::context_files::Skill;
use crate::memory::MemoryStore;
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
    pub writes: files::WriteTracker,
}

#[derive(Clone)]
pub(crate) enum Tool {
    Read,
    ListFiles,
    SearchFiles,
    WorkspaceOverview,
    Append,
    WebSearch,
    WebFetch,
    DocumentRead,
    DocumentEdit,
    Write,
    Edit,
    Bash,
    Skill,
    TaskMemory,
    External(ExternalTool),
    Context(ContextTool),
}

impl Tool {
    /// Catch common model argument mistakes before asking the user to approve
    /// an unusable call. Tool implementations still enforce semantic rules.
    pub(crate) fn validate_arguments(&self, arguments: &Value) -> Result<(), String> {
        if !arguments.is_object() {
            return Err("arguments must be a JSON object".into());
        }
        let definition = self.definition();
        let schema = &definition["function"]["parameters"];
        if let Some(required) = schema["required"].as_array() {
            for key in required.iter().filter_map(Value::as_str) {
                if arguments.get(key).is_none() {
                    return Err(format!("missing required argument `{key}`"));
                }
            }
        }
        if let Some(properties) = schema["properties"].as_object() {
            for (key, property) in properties {
                let Some(value) = arguments.get(key) else {
                    continue;
                };
                let valid = match property["type"].as_str() {
                    Some("string") => value.is_string(),
                    Some("integer") => value.is_i64() || value.is_u64(),
                    Some("number") => value.is_number(),
                    Some("boolean") => value.is_boolean(),
                    Some("object") => value.is_object(),
                    Some("array") => value.is_array(),
                    _ => true,
                };
                if !valid {
                    return Err(format!("`{key}` must have type {}", property["type"]));
                }
                if let Some(choices) = property["enum"].as_array() {
                    if !choices.contains(value) {
                        return Err(format!("`{key}` must be one of {}", property["enum"]));
                    }
                }
                for (bound, actual, lower) in [
                    ("minimum", value.as_f64(), true),
                    ("maximum", value.as_f64(), false),
                    (
                        "minLength",
                        value.as_str().map(|s| s.chars().count() as f64),
                        true,
                    ),
                    (
                        "maxLength",
                        value.as_str().map(|s| s.chars().count() as f64),
                        false,
                    ),
                    ("minItems", value.as_array().map(|a| a.len() as f64), true),
                    ("maxItems", value.as_array().map(|a| a.len() as f64), false),
                ] {
                    if let (Some(actual), Some(limit)) = (actual, property[bound].as_f64()) {
                        if (lower && actual < limit) || (!lower && actual > limit) {
                            return Err(format!("`{key}` violates {bound}={limit}"));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Read => "read",
            Self::ListFiles => "list_files",
            Self::SearchFiles => "search_files",
            Self::WorkspaceOverview => "workspace_overview",
            Self::Append => "append",
            Self::WebSearch => "web_search",
            Self::WebFetch => "web_fetch",
            Self::DocumentRead => "document_read",
            Self::DocumentEdit => "document_edit",
            Self::Write => "write",
            Self::Edit => "edit",
            Self::Bash => "bash",
            Self::Skill => "skill",
            Self::TaskMemory => "task_memory",
            Self::External(tool) => &tool.name,
            Self::Context(tool) => &tool.name,
        }
    }

    pub(crate) fn risk(&self) -> Risk {
        match self {
            Self::Read
            | Self::ListFiles
            | Self::SearchFiles
            | Self::WorkspaceOverview
            | Self::DocumentRead
            | Self::Skill
            | Self::TaskMemory => Risk::ReadOnly,
            Self::Write | Self::Edit | Self::Append | Self::DocumentEdit => Risk::Mutating,
            Self::Bash | Self::WebSearch | Self::WebFetch => Risk::Dangerous,
            Self::External(tool) => Risk::parse(&tool.risk),
            Self::Context(tool) => Risk::parse(&tool.risk),
        }
    }

    pub(crate) fn definition(&self) -> Value {
        match self {
            Self::WorkspaceOverview => overview::definition(),
            Self::ListFiles | Self::SearchFiles => {
                discovery::definition(matches!(self, Self::SearchFiles))
            }
            Self::Append => json!({
                "type": "function", "function": {
                    "name": "append",
                    "description": "Append exact text to a workspace text file, creating it and parent directories if missing. Does not add a newline automatically. Existing content is backed up under .drops/backups and the replacement is atomic. Requires user approval with a diff preview unless sudo mode is enabled.",
                    "parameters": {"type":"object", "properties": {
                        "path":{"type":"string"}, "content":{"type":"string"},
                        "expected_content":{"type":"string", "description":"Optional full previously read content. Refuse the append if the current file differs."}
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
                    "description": "Read a regular UTF-8 text file (up to 8 MiB) inside the project directory. Binary files and devices are rejected. Output is truncated for large files; use offset and limit to continue. A truncated single line requires another tool such as search_files to locate text beyond its prefix.",
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
                    "description": "Create or overwrite a regular UTF-8 text file inside the project directory. Parent directories are created automatically. An overwrite keeps a recoverable backup under .drops/backups and is applied atomically. Requires user approval with a diff preview unless sudo mode is enabled.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "Path relative to the project root." },
                            "content": { "type": "string", "description": "Full content to write." },
                            "expected_content": {"type":"string", "description":"Optional full previously read content. Refuse the overwrite if the current file differs."}
                        },
                        "required": ["path", "content"]
                    }
                }
            }),
            Self::Edit => json!({
                "type": "function",
                "function": {
                    "name": "edit",
                    "description": "Edit a workspace text file using a single exact replacement or a batch of non-overlapping edits against the original text. Batch edits are all validated before writing; a failed match leaves the file unchanged. Requires user approval unless sudo mode is enabled.",
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
                    "description": "Run a bash command in the project directory. The user must approve every invocation unless sudo mode is enabled. Returns combined output and the exit code.",
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
            Self::TaskMemory => json!({
                "type": "function",
                "function": {
                    "name": "task_memory",
                    "description": "Read or update bounded task memory for this Drop session only. Use `update` to record the current goal, decisions, pending work and verified results across turns. Lists you pass replace that list; omitted lists stay unchanged. Verified entries need evidence from a tool result or the conversation. Memory is not authorization and never grants file or web access.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "action": {"type":"string", "enum":["get","update","clear"], "description":"Use `get` to inspect, `update` to change fields, or `clear` to remove all task memory."},
                            "goal": {"type":"string", "maxLength":500, "description":"Current user goal. An empty string clears it."},
                            "decisions": {"type":"array", "maxItems":8, "items":memory_item_schema(), "description":"Replace the decisions list."},
                            "pending": {"type":"array", "maxItems":8, "items":memory_item_schema(), "description":"Replace the pending-work list."},
                            "verified": {"type":"array", "maxItems":8, "items":memory_item_schema_verified(), "description":"Replace the verified-results list. Every item needs evidence."}
                        },
                        "required": ["action"]
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
    memory: Option<Arc<MemoryStore>>,
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
            Tool::WorkspaceOverview,
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
                writes: files::WriteTracker::default(),
            },
            tools,
            memory: None,
        }
    }

    /// Register session-scoped task memory. Kept out of bare registries used by
    /// focused tool tests, so a registry only advertises the tool when a real
    /// session store is attached.
    pub(crate) fn with_memory(mut self, memory: Arc<MemoryStore>) -> Self {
        self.memory = Some(memory);
        // This built-in is attached after external/context registration. Give
        // it the same precedence as the other built-ins and avoid duplicate
        // function names in requests, including when a store is reattached.
        self.tools.retain(|tool| tool.name() != "task_memory");
        self.tools.push(Tool::TaskMemory);
        self
    }

    /// Read-only preview for the approval card. Errors here mean the call
    /// cannot be performed as written, so the engine reports them without
    /// asking for approval or executing anything.
    pub(crate) fn preview(&self, tool: &Tool, arguments: &Value) -> Result<Option<Value>, String> {
        match tool {
            Tool::Write => files::preview_write(&self.ctx, arguments),
            Tool::Edit => files::preview_edit(&self.ctx, arguments),
            Tool::Append => files::preview_append(&self.ctx, arguments),
            _ => Ok(None),
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
            Tool::WorkspaceOverview => overview::execute(&self.ctx, arguments, cancel),
            Tool::Append => files::append(&self.ctx, arguments),
            Tool::WebSearch => web::execute(arguments, true, cancel).await,
            Tool::WebFetch => web::execute(arguments, false, cancel).await,
            Tool::DocumentRead => office::execute(&self.ctx, arguments, false, cancel).await,
            Tool::DocumentEdit => office::execute(&self.ctx, arguments, true, cancel).await,
            Tool::Write => files::write(&self.ctx, arguments),
            Tool::Edit => files::edit(&self.ctx, arguments),
            Tool::Bash => shell::bash(&self.ctx, arguments, cancel).await,
            Tool::Skill => skill(&self.ctx, arguments),
            Tool::TaskMemory => match &self.memory {
                Some(memory) => match memory.execute(arguments, &self.ctx.root) {
                    Ok(content) => ToolOutcome::ok(content),
                    Err(error) => ToolOutcome::error(format!("task_memory: {error}")),
                },
                None => ToolOutcome::error("task_memory: no session memory is attached"),
            },
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

fn memory_item_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "text": {"type":"string", "minLength":1, "maxLength":240},
            "paths": {"type":"array", "maxItems":8, "items":{"type":"string"}, "description":"Workspace-relative paths this item depends on."},
            "anchor_scoped": {"type":"boolean", "description":"True when the item depends on the current window content."}
        },
        "required": ["text"]
    })
}

fn memory_item_schema_verified() -> Value {
    let mut schema = memory_item_schema();
    schema["properties"]["evidence"] = json!({"type":"string", "minLength":1, "maxLength":240});
    schema["required"] = json!(["text", "evidence"]);
    schema
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
    fn session_memory_wins_name_collisions_and_is_registered_once() {
        let temp = crate::testutil::TempDir::new();
        let memory = Arc::new(MemoryStore::load(crate::session::SessionLog::new(
            temp.path().join("session.jsonl"),
        )));
        let external = ExternalTool {
            name: "task_memory".into(),
            description: "shadow".into(),
            command: vec!["/bin/false".into()],
            parameters: json!({"type":"object"}),
            risk: "dangerous".into(),
        };
        let registry = ToolRegistry::new(temp.path().into(), false, vec![], vec![external], vec![])
            .with_memory(memory.clone())
            .with_memory(memory);
        assert!(matches!(
            registry.find("task_memory"),
            Some(Tool::TaskMemory)
        ));
        assert_eq!(
            registry
                .names()
                .iter()
                .filter(|name| **name == "task_memory")
                .count(),
            1
        );
    }

    #[test]
    fn basic_argument_checks_reject_invalid_calls_and_accept_valid_ones() {
        assert!(Tool::Read
            .validate_arguments(&json!({"path":"a", "limit":2}))
            .is_ok());
        for args in [
            json!([]),
            json!({}),
            json!({"path":42}),
            json!({"path":"a", "limit":0}),
            json!({"path":"a", "limit":2.5}),
        ] {
            assert!(Tool::Read.validate_arguments(&args).is_err(), "{args}");
        }
        assert!(Tool::SearchFiles
            .validate_arguments(&json!({"query":""}))
            .is_err());
        assert!(Tool::SearchFiles
            .validate_arguments(&json!({"query":"界".repeat(512)}))
            .is_ok());
        assert!(Tool::SearchFiles
            .validate_arguments(&json!({"query":"界".repeat(513)}))
            .is_err());
        assert!(Tool::Edit
            .validate_arguments(&json!({"path":"a", "edits":[]}))
            .is_err());
        let external = Tool::External(ExternalTool {
            name: "choice".into(),
            description: "choice".into(),
            command: vec![],
            risk: "dangerous".into(),
            parameters: json!({"type":"object", "properties":{"mode":{"type":"string", "enum":["a", "b"]}}}),
        });
        assert!(external.validate_arguments(&json!({"mode":"a"})).is_ok());
        assert!(external.validate_arguments(&json!({"mode":"c"})).is_err());
    }

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
                "workspace_overview",
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
        for name in [
            "list_files",
            "search_files",
            "workspace_overview",
            "document_read",
        ] {
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
