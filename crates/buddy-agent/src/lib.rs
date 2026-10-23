//! Buddy/Drops agent engine.
//!
//! This crate owns the model-independent half of the agent: the tool loop,
//! project-root confinement, approvals, cancellation, external stdio tools,
//! layered markdown instructions and JSONL sessions. The provider (one
//! OpenAI-compatible chat-completions request) lives in `buddy-ai`.
//!
//! The public surface is exactly the one described in `docs/contracts.md`:
//! [`AgentConfig`], [`ExternalTool`], [`run`] and [`session_path`]. Everything
//! else is private so the root workspace can integrate the engine without
//! depending on implementation details.
//!
//! Design notes and the Pi attribution are in `ATTRIBUTION.md`.

mod attachments;
mod context_files;
mod events;
mod paths;
mod provider;
mod session;
#[cfg(test)]
pub(crate) mod testutil;
mod tools;

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::context_files::Skill;
use crate::session::{CallOutcome, ParsedCall, SessionLog};
use crate::tools::{Risk, ToolRegistry};

/// Everything the engine needs for one run.
///
/// `session_dir` is where `<session_id>.jsonl` lives; `project_dir` is the
/// canonical sandbox root for read/write/edit (and the working directory for
/// bash and external tools). `instructions` is the base system prompt supplied
/// by the daemon; layered `AGENTS.md`/`.drops/*.md` files are appended to it.
#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub provider: buddy_ai::ProviderConfig,
    pub session_dir: PathBuf,
    pub project_dir: PathBuf,
    pub instructions: String,
    pub max_turns: usize,
    pub external_tools: Vec<ExternalTool>,
}

/// A user-configured executable that speaks the external tool protocol.
///
/// The engine runs `command` (argv vector, never a shell string) in the project
/// root, writes one JSON request line to stdin, and reads one JSON response
/// from stdout. See `ATTRIBUTION.md` for the exact protocol.
///
/// `risk` is one of `read-only`, `mutating` or `dangerous`. It is treated
/// fail-safe: an unrecognized risk is dangerous and always needs approval.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExternalTool {
    pub name: String,
    pub description: String,
    pub command: Vec<String>,
    #[serde(default = "default_tool_parameters")]
    pub parameters: Value,
    #[serde(default = "default_tool_risk")]
    pub risk: String,
}

fn default_tool_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": true
    })
}

fn default_tool_risk() -> String {
    // Fail safe: a tool that did not declare a risk is treated as dangerous.
    "dangerous".to_string()
}

/// Path of a session log, with the session id validated against traversal.
///
/// Valid ids are 1..=128 ASCII characters, start with a letter or digit, and
/// contain only letters, digits, `-`, `_` and `.`. `..`, path separators and
/// whitespace are rejected, so the result is always a direct child of `dir`.
pub fn session_path(dir: &Path, id: &str) -> Result<PathBuf> {
    session::validate_session_id(id)?;
    Ok(dir.join(format!("{id}.jsonl")))
}

/// Run one agent turn sequence.
///
/// `context` is the (possibly empty) context bundle for this request; it is
/// recorded in the session and sent to the model as a user message. The engine
/// loads prior JSONL history, appends the context/user prompt, then loops:
/// stream an assistant message, execute tool calls, append tool results, and
/// repeat until the model stops calling tools or `max_turns` is reached.
///
/// Events are emitted as JSON objects with a `type` field (`text`, `thinking`,
/// `tool_start`, `tool_result`, `approval`, `error`, `done`). A `done` event is
/// emitted at the end of every accepted run, including cancellation and
/// provider failure.
///
/// Approvals are consumed one `bool` per tool that needs approval. Denial,
/// a closed approvals channel, or cancellation means the action is not
/// performed; there is no bypass.
// The eight-argument signature is fixed by docs/contracts.md.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    config: AgentConfig,
    session_id: String,
    prompt: String,
    context: Value,
    allow_shell: bool,
    events: mpsc::UnboundedSender<Value>,
    approvals: mpsc::UnboundedReceiver<bool>,
    cancel: CancellationToken,
) -> Result<()> {
    let path = match session_path(&config.session_dir, &session_id) {
        Ok(path) => path,
        Err(err) => {
            events::error(&events, format!("session error: {err:#}"));
            events::done(&events);
            return Err(err);
        }
    };
    if let Err(err) = std::fs::create_dir_all(&config.session_dir) {
        let err = anyhow::Error::new(err).context(format!(
            "creating session directory {}",
            config.session_dir.display()
        ));
        events::error(&events, format!("session error: {err:#}"));
        events::done(&events);
        return Err(err);
    }
    let root = match paths::canonical_root(&config.project_dir) {
        Ok(root) => root,
        Err(err) => {
            events::error(&events, format!("session error: {err:#}"));
            events::done(&events);
            return Err(err);
        }
    };
    let log = SessionLog::new(path);

    // One run per session file at a time, so concurrent daemon requests cannot
    // interleave partial JSONL lines. The guard lives for the whole run.
    let _session_guard = session::lock(log.path()).await;

    let skills = context_files::discover_skills(&root);
    let registry = ToolRegistry::new(
        root.clone(),
        allow_shell,
        config.external_tools.clone(),
        skills.clone(),
    );

    let mut engine = Engine {
        config,
        root,
        log,
        registry,
        skills,
        messages: Vec::new(),
        context,
        events,
        approvals,
        cancel,
        allow_shell,
    };
    engine.drive(&session_id, prompt).await
}

enum Stop {
    Finished,
    Cancelled,
}

struct Engine {
    config: AgentConfig,
    root: PathBuf,
    log: SessionLog,
    registry: ToolRegistry,
    skills: Vec<Skill>,
    messages: Vec<Value>,
    context: Value,
    events: mpsc::UnboundedSender<Value>,
    approvals: mpsc::UnboundedReceiver<bool>,
    cancel: CancellationToken,
    allow_shell: bool,
}

enum Approval {
    Approved,
    Denied,
    Cancelled,
}

impl Engine {
    async fn drive(&mut self, session_id: &str, prompt: String) -> Result<()> {
        if self.cancel.is_cancelled() {
            events::error(&self.events, "cancelled");
            events::done(&self.events);
            return Ok(());
        }

        if let Err(err) = self.prepare(session_id, prompt) {
            if self.cancel.is_cancelled() {
                events::error(&self.events, "cancelled");
            } else {
                events::error(&self.events, format!("session error: {err:#}"));
            }
            events::done(&self.events);
            return Err(err);
        }

        let max_turns = self.config.max_turns.max(1);
        let mut turns = 0usize;
        let mut stop: Option<Stop> = None;

        while turns < max_turns {
            if self.cancel.is_cancelled() {
                stop = Some(Stop::Cancelled);
                break;
            }
            turns += 1;

            let assistant = match self.model_turn().await {
                Ok(assistant) => assistant,
                Err(err) => {
                    let cancelled = self.cancel.is_cancelled() || buddy_ai::is_cancelled(&err);
                    if cancelled {
                        stop = Some(Stop::Cancelled);
                    } else {
                        events::error(&self.events, format!("provider error: {err:#}"));
                        events::done(&self.events);
                        return Err(err);
                    }
                    break;
                }
            };

            let calls = session::tool_calls(&assistant);
            if calls.is_empty() {
                stop = Some(Stop::Finished);
                break;
            }

            match self.run_calls(calls).await {
                Ok(true) => {
                    stop = Some(Stop::Cancelled);
                    break;
                }
                Ok(false) => {}
                Err(err) => {
                    events::error(&self.events, format!("session error: {err:#}"));
                    events::done(&self.events);
                    return Err(err);
                }
            }
        }

        match stop {
            Some(Stop::Finished) => {}
            Some(Stop::Cancelled) => events::error(&self.events, "cancelled"),
            None => events::error(
                &self.events,
                format!("maximum number of turns reached ({max_turns}); stopping without a final answer"),
            ),
        }
        events::done(&self.events);
        Ok(())
    }

    fn prepare(&mut self, session_id: &str, prompt: String) -> Result<()> {
        self.log.ensure_header(session_id, &self.root)?;
        let system = context_files::compose_system(
            &self.config.instructions,
            &self.root,
            &self.skills,
            self.allow_shell,
            self.config.provider.tools,
        );
        self.messages
            .push(json!({ "role": "system", "content": system }));

        // A crash (or SIGINT the daemon could not join) can leave an
        // assistant message whose tool results were never written. Strict
        // endpoints reject dangling `tool_calls`, so close those batches and
        // persist the synthetic results before adding the next user turn.
        let repaired = session::repair_history(self.log.load()?);
        for message in &repaired.appended {
            self.log.append_message(message)?;
        }
        let history = session::trim_history(repaired.messages);
        self.messages.extend(history);

        if !session::is_empty_context(&self.context) {
            let message = attachments::context_message(&self.context, self.config.provider.vision);
            self.log.append_message(&message)?;
            self.messages.push(message);
        }

        let user = json!({ "role": "user", "content": prompt });
        self.log.append_message(&user)?;
        self.messages.push(user);
        Ok(())
    }

    async fn model_turn(&mut self) -> Result<Value> {
        let definitions = if self.config.provider.tools {
            self.registry.definitions()
        } else {
            Vec::new()
        };
        let wire: Vec<Value> = self
            .messages
            .iter()
            .filter_map(session::wire_message)
            .map(|m| attachments::for_provider(m, self.config.provider.vision))
            .collect();

        let (assistant, streamed) = provider::complete(
            &self.config.provider,
            &wire,
            &definitions,
            &self.events,
            &self.cancel,
        )
        .await?;

        let assistant = session::normalize_assistant(assistant);
        // Providers normally stream text deltas. For non-streaming or broken
        // servers, show the final content exactly once.
        if !streamed.saw_text {
            if let Some(text) = assistant.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    events::text(&self.events, text);
                }
            }
        }
        if !streamed.saw_thinking {
            if let Some(text) = assistant.get("reasoning_content").and_then(Value::as_str) {
                if !text.is_empty() {
                    events::thinking(&self.events, text);
                }
            }
        }

        self.log.append_message(&assistant)?;
        self.messages.push(assistant.clone());
        Ok(assistant)
    }

    /// Execute one assistant message's tool calls in order. Returns `true` when
    /// cancellation interrupted the batch.
    ///
    /// Every call in the batch gets exactly one recorded result, including
    /// calls that cancellation skipped, so a graceful stop never leaves a
    /// dangling `tool_calls` entry for the next turn.
    async fn run_calls(&mut self, calls: Vec<ParsedCall>) -> Result<bool> {
        let mut pending = calls.into_iter();
        while let Some(call) = pending.next() {
            if self.cancel.is_cancelled() {
                self.append_cancelled(&call)?;
                for call in pending {
                    self.append_cancelled(&call)?;
                }
                return Ok(true);
            }

            let name = call.name.clone().unwrap_or_default();
            events::tool_start(&self.events, &call.id, &name, &call.display_arguments);

            match self.execute_call(&call).await {
                CallOutcome::Cancelled => {
                    self.append_cancelled(&call)?;
                    for call in pending {
                        self.append_cancelled(&call)?;
                    }
                    return Ok(true);
                }
                CallOutcome::Finished { content, is_error } => {
                    events::tool_result(&self.events, &call.id, &name, &content, is_error);
                    let message = call.tool_message(&content, is_error);
                    self.log.append_message(&message)?;
                    self.messages.push(message);
                }
            }
        }
        Ok(false)
    }

    fn append_cancelled(&mut self, call: &ParsedCall) -> Result<()> {
        let message = call.tool_message("cancelled by user", true);
        self.log.append_message(&message)?;
        self.messages.push(message);
        Ok(())
    }

    async fn execute_call(&mut self, call: &ParsedCall) -> CallOutcome {
        if !self.config.provider.tools {
            return CallOutcome::Finished {
                content: "Tools are disabled for this session (provider.tools=false), so no tool was executed. Answer using the conversation and supplied context only.".to_string(),
                is_error: true,
            };
        }

        let name = match call.name.as_deref() {
            Some(name) if !name.trim().is_empty() => name,
            _ => {
                return CallOutcome::Finished {
                    content: "The model produced a tool call without a tool name. Re-issue the call with a valid tool name.".to_string(),
                    is_error: true,
                }
            }
        };

        let arguments = match &call.arguments {
            Ok(arguments) => arguments.clone(),
            Err(err) => {
                return CallOutcome::Finished {
                    content: format!(
                        "Tool call `{name}` has invalid JSON arguments: {err}. Re-issue the call with a complete JSON object."
                    ),
                    is_error: true,
                }
            }
        };

        let Some(tool) = self.registry.find(name).cloned() else {
            let available = self.registry.names().join(", ");
            return CallOutcome::Finished {
                content: format!(
                    "Unknown tool `{name}`. Available tools: {available}. Re-issue the call using one of those names."
                ),
                is_error: true,
            };
        };

        let risk = tool.risk();
        if risk.requires_approval() {
            match self.approval(name, &arguments, risk).await {
                Approval::Approved => {}
                Approval::Denied => {
                    return CallOutcome::Finished {
                        content: format!(
                            "The user denied approval for `{name}`. The action was not performed. Do not retry it unless the user asks."
                        ),
                        is_error: true,
                    }
                }
                Approval::Cancelled => return CallOutcome::Cancelled,
            }
        }

        let outcome = self.registry.execute(&tool, &arguments, &self.cancel).await;
        if self.cancel.is_cancelled() && outcome.is_error {
            return CallOutcome::Cancelled;
        }
        CallOutcome::Finished {
            content: outcome.content,
            is_error: outcome.is_error,
        }
    }

    async fn approval(&mut self, name: &str, arguments: &Value, risk: Risk) -> Approval {
        events::approval(&self.events, name, arguments, risk.as_str());
        tokio::select! {
            _ = self.cancel.cancelled() => Approval::Cancelled,
            answer = self.approvals.recv() => match answer {
                Some(true) => Approval::Approved,
                // Default deny: false, a closed channel, or a missing answer.
                _ => Approval::Denied,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::Duration;

    use super::*;
    use crate::provider::test_support;
    use crate::testutil::TempDir;

    fn setup(max_turns: usize) -> (TempDir, AgentConfig) {
        let temp = TempDir::new();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let config = AgentConfig {
            provider: buddy_ai::ProviderConfig {
                model: "test-model".into(),
                ..Default::default()
            },
            session_dir: temp.path().join("sessions"),
            project_dir: project,
            instructions: "Test instructions.".into(),
            max_turns,
            external_tools: Vec::new(),
        };
        (temp, config)
    }

    fn text(content: &str) -> Value {
        json!({ "role": "assistant", "content": content })
    }

    fn call(id: &str, name: &str, arguments: Value) -> Value {
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": id,
                "type": "function",
                "function": { "name": name, "arguments": arguments.to_string() }
            }]
        })
    }

    fn two_calls(first: (&str, &str, Value), second: (&str, &str, Value)) -> Value {
        let (id1, name1, args1) = first;
        let (id2, name2, args2) = second;
        json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [
                { "id": id1, "type": "function", "function": { "name": name1, "arguments": args1.to_string() } },
                { "id": id2, "type": "function", "function": { "name": name2, "arguments": args2.to_string() } }
            ]
        })
    }

    fn approvals(
        values: Vec<bool>,
    ) -> (mpsc::UnboundedSender<bool>, mpsc::UnboundedReceiver<bool>) {
        let (tx, rx) = mpsc::unbounded_channel();
        for value in values {
            tx.send(value).unwrap();
        }
        (tx, rx)
    }

    fn drain(mut rx: mpsc::UnboundedReceiver<Value>) -> Vec<Value> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn history(config: &AgentConfig, id: &str) -> Vec<Value> {
        let path = session_path(&config.session_dir, id).unwrap();
        crate::session::SessionLog::new(path).load().unwrap()
    }

    fn seed(config: &AgentConfig, id: &str, messages: &[Value]) {
        std::fs::create_dir_all(&config.session_dir).unwrap();
        let path = session_path(&config.session_dir, id).unwrap();
        let log = crate::session::SessionLog::new(path);
        log.ensure_header(id, &config.project_dir).unwrap();
        for message in messages {
            log.append_message(message).unwrap();
        }
    }

    fn strict_history(config: &AgentConfig, id: &str) {
        let messages = history(config, id);
        let wire: Vec<Value> = messages
            .iter()
            .filter_map(crate::session::wire_message)
            .collect();
        crate::testutil::assert_strict_wire(&wire);
    }

    fn event_types(events: &[Value]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| event["type"].as_str())
            .collect()
    }

    #[tokio::test]
    async fn executes_read_only_tool_and_records_jsonl() {
        let (_temp, config) = setup(4);
        std::fs::write(config.project_dir.join("hello.txt"), "hello from disk").unwrap();

        let mut turn = 0;
        let _mock = test_support::install(move |_messages, tools| {
            turn += 1;
            match turn {
                1 => {
                    assert!(tools.iter().any(|tool| tool["function"]["name"] == "read"));
                    Ok(call("c1", "read", json!({"path":"hello.txt"})))
                }
                _ => Ok(text("done")),
            }
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        run(
            config.clone(),
            "s1".into(),
            "read the file".into(),
            json!({"window":{"class":"kitty"}}),
            false,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let events = drain(rx);
        let types = event_types(&events);
        assert!(types.contains(&"tool_start"));
        assert!(types.contains(&"tool_result"));
        assert!(types.contains(&"text"));
        assert_eq!(types.last(), Some(&"done"));
        assert!(!types.contains(&"approval"), "read is read-only");

        let result = events
            .iter()
            .find(|event| event["type"] == "tool_result")
            .unwrap();
        assert_eq!(result["name"], "read");
        assert_eq!(result["is_error"], false);
        assert!(result["text"].as_str().unwrap().contains("hello from disk"));

        let messages = history(&config, "s1");
        let roles: Vec<_> = messages
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            roles,
            vec!["user", "user", "assistant", "tool", "assistant"]
        );
        assert!(messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("Context for this request"));
        assert_eq!(messages[2]["tool_calls"][0]["id"], "c1");
        assert_eq!(messages[3]["tool_call_id"], "c1");
        assert_eq!(messages.last().unwrap()["content"], "done");
    }

    #[tokio::test]
    async fn write_requires_exact_approval_and_denial_is_reported() {
        let (_temp, config) = setup(4);
        let mut turn = 0;
        let _mock = test_support::install(move |_messages, _tools| {
            turn += 1;
            match turn {
                1 => Ok(call(
                    "c1",
                    "write",
                    json!({"path":"new.txt","content":"secret"}),
                )),
                _ => Ok(text("understood")),
            }
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = approvals(vec![false]);
        let result = run(
            config.clone(),
            "denied".into(),
            "write a file".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await;
        assert!(result.is_ok());

        let events = drain(rx);
        let approval = events
            .iter()
            .find(|event| event["type"] == "approval")
            .expect("approval event");
        assert_eq!(approval["name"], "write");
        assert_eq!(approval["risk"], "mutating");
        assert_eq!(
            approval["arguments"],
            json!({"path":"new.txt","content":"secret"})
        );

        let tool_result = events
            .iter()
            .find(|event| event["type"] == "tool_result")
            .unwrap();
        assert_eq!(tool_result["is_error"], true);
        assert!(tool_result["text"].as_str().unwrap().contains("denied"));

        assert!(!config.project_dir.join("new.txt").exists());
        let messages = history(&config, "denied");
        let tool_message = messages
            .iter()
            .find(|message| message["role"] == "tool")
            .unwrap();
        assert!(tool_message["content"]
            .as_str()
            .unwrap()
            .starts_with("Error:"));
    }

    #[tokio::test]
    async fn approves_once_per_tool_call() {
        let (_temp, config) = setup(4);
        let mut turn = 0;
        let _mock = test_support::install(move |_messages, _tools| {
            turn += 1;
            match turn {
                1 => Ok(two_calls(
                    ("c1", "write", json!({"path":"a.txt","content":"a"})),
                    ("c2", "write", json!({"path":"b.txt","content":"b"})),
                )),
                _ => Ok(text("done")),
            }
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = approvals(vec![true, false]);
        run(
            config.clone(),
            "two".into(),
            "write two files".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let events = drain(rx);
        assert_eq!(
            events
                .iter()
                .filter(|event| event["type"] == "approval")
                .count(),
            2
        );
        assert!(config.project_dir.join("a.txt").is_file());
        assert!(!config.project_dir.join("b.txt").exists());
    }

    #[tokio::test]
    async fn unknown_tool_and_malformed_arguments_are_model_errors() {
        let (_temp, config) = setup(5);
        let mut turn = 0;
        let _mock = test_support::install(move |_messages, _tools| {
            turn += 1;
            match turn {
                1 => Ok(call("c1", "does_not_exist", json!({}))),
                2 => Ok(json!({
                    "role":"assistant",
                    "content":"",
                    "tool_calls":[{
                        "id":"c2",
                        "type":"function",
                        "function":{"name":"read","arguments":"{\"path\":"}
                    }]
                })),
                _ => Ok(text("recovered")),
            }
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        run(
            config.clone(),
            "bad".into(),
            "try broken calls".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let events = drain(rx);
        let results: Vec<_> = events
            .iter()
            .filter(|event| event["type"] == "tool_result")
            .collect();
        assert_eq!(results.len(), 2);
        assert!(results[0]["text"]
            .as_str()
            .unwrap()
            .contains("Unknown tool"));
        assert!(results[1]["text"]
            .as_str()
            .unwrap()
            .contains("invalid JSON arguments"));
        assert!(results.iter().all(|result| result["is_error"] == true));
        assert_eq!(events.last().unwrap()["type"], "done");
    }

    #[tokio::test]
    async fn max_turns_stops_with_error_and_done() {
        let (_temp, config) = setup(2);
        std::fs::write(config.project_dir.join("hello.txt"), "x").unwrap();
        let mut counter = 0;
        let _mock = test_support::install(move |_messages, _tools| {
            counter += 1;
            Ok(call(
                &format!("c{counter}"),
                "read",
                json!({"path":"hello.txt"}),
            ))
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        run(
            config.clone(),
            "limit".into(),
            "loop forever".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let events = drain(rx);
        let error = events
            .iter()
            .find(|event| event["type"] == "error")
            .unwrap();
        assert!(error["text"].as_str().unwrap().contains("maximum number"));
        assert_eq!(events.last().unwrap()["type"], "done");

        let messages = history(&config, "limit");
        assert_eq!(messages.len(), 5);
        assert_eq!(
            messages
                .iter()
                .filter(|message| message["role"] == "tool")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn cancellation_while_waiting_for_approval_stops_without_running() {
        let (_temp, config) = setup(4);
        let _mock = test_support::install(move |_messages, _tools| {
            Ok(call(
                "c1",
                "write",
                json!({"path":"never.txt","content":"nope"}),
            ))
        });

        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        run(
            config.clone(),
            "cancel".into(),
            "write".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            token,
        )
        .await
        .unwrap();

        let events = drain(rx);
        let types = event_types(&events);
        assert!(types.contains(&"approval"));
        assert!(types.contains(&"error"));
        assert_eq!(events.last().unwrap()["type"], "done");
        assert!(events
            .iter()
            .any(|event| event["type"] == "error" && event["text"] == "cancelled"));
        assert!(!config.project_dir.join("never.txt").exists());
    }

    #[tokio::test]
    async fn cancellation_before_start_emits_done() {
        let (_temp, config) = setup(4);
        let token = CancellationToken::new();
        token.cancel();

        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        run(
            config,
            "precancel".into(),
            "do nothing".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            token,
        )
        .await
        .unwrap();

        let events = drain(rx);
        assert!(events
            .iter()
            .any(|event| event["type"] == "error" && event["text"] == "cancelled"));
        assert_eq!(events.last().unwrap()["type"], "done");
    }

    #[tokio::test]
    async fn bash_is_absent_without_allow_shell() {
        let (_temp, config) = setup(4);
        let mut turn = 0;
        let _mock = test_support::install(move |_messages, tools| {
            turn += 1;
            match turn {
                1 => {
                    assert!(!tools.iter().any(|tool| tool["function"]["name"] == "bash"));
                    Ok(call("c1", "bash", json!({"command":"echo hi"})))
                }
                _ => Ok(text("understood")),
            }
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        run(
            config,
            "noshell".into(),
            "run a command".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let events = drain(rx);
        assert!(!event_types(&events).contains(&"approval"));
        let result = events
            .iter()
            .find(|event| event["type"] == "tool_result")
            .unwrap();
        assert_eq!(result["is_error"], true);
        assert!(result["text"].as_str().unwrap().contains("Unknown tool"));
    }

    #[tokio::test]
    async fn bash_is_dangerous_and_needs_approval() {
        let (_temp, config) = setup(4);
        let mut turn = 0;
        let _mock = test_support::install(move |_messages, _tools| {
            turn += 1;
            match turn {
                1 => Ok(call("c1", "bash", json!({"command":"printf hi"}))),
                _ => Ok(text("done")),
            }
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = approvals(vec![true]);
        run(
            config,
            "shell".into(),
            "run printf".into(),
            json!({}),
            true,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let events = drain(rx);
        let approval = events
            .iter()
            .find(|event| event["type"] == "approval")
            .unwrap();
        assert_eq!(approval["name"], "bash");
        assert_eq!(approval["risk"], "dangerous");
        assert_eq!(approval["arguments"], json!({"command":"printf hi"}));

        let result = events
            .iter()
            .find(|event| event["type"] == "tool_result")
            .unwrap();
        assert_eq!(result["is_error"], false);
        assert!(result["text"].as_str().unwrap().contains("hi"));
    }

    #[tokio::test]
    async fn tool_calls_are_rejected_when_provider_tools_is_false() {
        let (_temp, mut config) = setup(4);
        config.provider.tools = false;
        let mut turn = 0;
        let _mock = test_support::install(move |_messages, tools| {
            turn += 1;
            match turn {
                1 => {
                    assert!(tools.is_empty(), "tool definitions must be empty");
                    Ok(call(
                        "c1",
                        "write",
                        json!({"path":"unsolicited.txt","content":"nope"}),
                    ))
                }
                _ => Ok(text("no tools used")),
            }
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        run(
            config.clone(),
            "notools".into(),
            "do not use tools".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let events = drain(rx);
        assert!(!event_types(&events).contains(&"approval"));
        let result = events
            .iter()
            .find(|event| event["type"] == "tool_result")
            .unwrap();
        assert_eq!(result["is_error"], true);
        assert!(result["text"]
            .as_str()
            .unwrap()
            .contains("Tools are disabled"));
        assert!(!config.project_dir.join("unsolicited.txt").exists());
        assert_eq!(events.last().unwrap()["type"], "done");
    }

    #[tokio::test]
    async fn invalid_session_id_emits_error_and_done() {
        let (_temp, config) = setup(1);
        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        let result = run(
            config,
            "../escape".into(),
            "hello".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await;
        assert!(result.is_err());
        let events = drain(rx);
        assert!(events.iter().any(|event| event["type"] == "error"
            && event["text"].as_str().unwrap().contains("session error")));
        assert_eq!(events.last().unwrap()["type"], "done");
    }

    #[tokio::test]
    async fn provider_error_emits_error_and_done_then_returns_err() {
        let (_temp, config) = setup(4);
        let _mock = test_support::install(move |_messages, _tools| Err(anyhow::anyhow!("boom")));

        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        let result = run(
            config,
            "provider".into(),
            "hello".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await;

        assert!(result.is_err());
        let events = drain(rx);
        assert!(events.iter().any(|event| event["type"] == "error"
            && event["text"].as_str().unwrap().contains("provider error")));
        assert_eq!(events.last().unwrap()["type"], "done");
    }

    /// Simulate a crash between the assistant's `tool_calls` line and its
    /// results: the next request must still be strict-wire valid.
    #[tokio::test]
    async fn recovers_from_crash_mid_tool_call_and_persists_the_result() {
        let (_temp, config) = setup(4);
        seed(
            &config,
            "crash",
            &[
                json!({"role":"user","content":"Context for this request (JSON):\n{}"}),
                json!({"role":"user","content":"read the file"}),
                call("call_1", "read", json!({"path":"hello.txt"})),
            ],
        );
        let path = session_path(&config.session_dir, "crash").unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let mut turn = 0;
        let _mock = test_support::install(move |messages, _tools| {
            turn += 1;
            assert_eq!(turn, 1);
            crate::testutil::assert_strict_wire(messages);
            let replies: Vec<_> = messages
                .iter()
                .filter(|message| message["role"] == "tool")
                .collect();
            assert_eq!(replies.len(), 1);
            assert_eq!(replies[0]["tool_call_id"], "call_1");
            assert!(replies[0]["content"]
                .as_str()
                .unwrap()
                .contains("interrupted"));
            Ok(text("recovered"))
        });

        let (tx, _rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        run(
            config.clone(),
            "crash".into(),
            "continue".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.starts_with(&before),
            "repair must append, never rewrite, the session log"
        );
        let messages = history(&config, "crash");
        let roles: Vec<_> = messages
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            roles,
            vec!["user", "user", "assistant", "tool", "user", "assistant"]
        );
        assert_eq!(messages[3]["tool_call_id"], "call_1");
        assert!(messages[3]["content"]
            .as_str()
            .unwrap()
            .contains("interrupted"));
        strict_history(&config, "crash");

        // The persisted result satisfies repair on every later turn: no second
        // synthetic result and no log churn.
        let _mock = test_support::install(|messages, _tools| {
            crate::testutil::assert_strict_wire(messages);
            Ok(text("second"))
        });
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        run(
            config.clone(),
            "crash".into(),
            "again".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let after_second = std::fs::read_to_string(&path).unwrap();
        assert!(after_second.starts_with(&after));
        let messages = history(&config, "crash");
        assert_eq!(
            messages
                .iter()
                .filter(|message| message["tool_call_id"] == "call_1")
                .count(),
            1
        );
        assert_eq!(
            messages
                .iter()
                .filter(|message| message["content"]
                    .as_str()
                    .map(|content| content.contains("interrupted"))
                    .unwrap_or(false))
                .count(),
            1
        );
        strict_history(&config, "crash");
    }

    /// A crash after only some results of a multi-call batch must keep the
    /// real results and synthesize only the missing ones.
    #[tokio::test]
    async fn recovers_from_crash_mid_batch() {
        let (_temp, config) = setup(4);
        let batch = two_calls(
            ("c1", "read", json!({"path":"a.txt"})),
            ("c2", "read", json!({"path":"b.txt"})),
        );
        seed(
            &config,
            "midbatch",
            &[
                json!({"role":"user","content":"read both"}),
                batch,
                json!({"role":"tool","tool_call_id":"c1","content":"partial result"}),
            ],
        );

        let _mock = test_support::install(|messages, _tools| {
            crate::testutil::assert_strict_wire(messages);
            let replies: Vec<_> = messages
                .iter()
                .filter(|message| message["role"] == "tool")
                .collect();
            assert_eq!(replies.len(), 2);
            assert_eq!(replies[0]["tool_call_id"], "c1");
            assert_eq!(replies[0]["content"], "partial result");
            assert_eq!(replies[1]["tool_call_id"], "c2");
            assert!(replies[1]["content"]
                .as_str()
                .unwrap()
                .contains("interrupted"));
            Ok(text("done"))
        });

        let (tx, _rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        run(
            config.clone(),
            "midbatch".into(),
            "continue".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let messages = history(&config, "midbatch");
        let replies: Vec<_> = messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .collect();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["tool_call_id"], "c1");
        assert_eq!(replies[0]["content"], "partial result");
        assert_eq!(replies[1]["tool_call_id"], "c2");
        strict_history(&config, "midbatch");
    }

    /// A truncated line in the middle of the log is skipped by `load`, which
    /// can leave a dangling batch mid-history. Repair must fix it in memory on
    /// every turn without churning the append-only log.
    #[tokio::test]
    async fn repairs_a_batch_interrupted_by_a_corrupt_middle_line() {
        let (_temp, config) = setup(4);
        let batch = two_calls(
            ("c1", "read", json!({"path":"a.txt"})),
            ("c2", "read", json!({"path":"b.txt"})),
        );
        seed(
            &config,
            "midlog",
            &[
                json!({"role":"user","content":"read both"}),
                batch,
                json!({"role":"tool","tool_call_id":"c1","content":"partial result"}),
            ],
        );
        let path = session_path(&config.session_dir, "midlog").unwrap();
        // Truncated append for c2's result, followed by later turns.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"message\",\"timestamp\":1,\"message\":{\"role\":\"tool\",\"tool_call_id\":\"c2\"\n")
            .unwrap();
        seed(
            &config,
            "midlog",
            &[
                json!({"role":"user","content":"follow-up"}),
                json!({"role":"assistant","content":"prior answer"}),
            ],
        );
        let before = std::fs::read_to_string(&path).unwrap();

        for prompt in ["continue", "again"] {
            let _mock = test_support::install(|messages, _tools| {
                crate::testutil::assert_strict_wire(messages);
                let replies: Vec<_> = messages
                    .iter()
                    .filter(|message| message["tool_call_id"] == "c2")
                    .collect();
                assert_eq!(replies.len(), 1);
                assert!(replies[0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("interrupted"));
                Ok(text("ok"))
            });
            let (tx, _rx) = mpsc::unbounded_channel();
            let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
            run(
                config.clone(),
                "midlog".into(),
                prompt.into(),
                json!({}),
                false,
                tx,
                approval_rx,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        }

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(&before));
        assert!(
            !after.contains("interrupted"),
            "mid-history repair must not append a result that cannot sit next to its call"
        );
        let repaired = crate::session::repair_history(history(&config, "midlog"));
        assert!(repaired.appended.is_empty());
        let wire: Vec<Value> = repaired
            .messages
            .iter()
            .filter_map(crate::session::wire_message)
            .collect();
        crate::testutil::assert_strict_wire(&wire);
        assert_eq!(
            repaired
                .messages
                .iter()
                .filter(|message| message["tool_call_id"] == "c2")
                .count(),
            1
        );
    }

    /// A graceful stop in the middle of a batch records a terminal result for
    /// every call, so even a clean shutdown never leaves dangling tool_calls.
    #[tokio::test]
    async fn graceful_cancellation_closes_every_call_in_the_batch() {
        let (_temp, config) = setup(4);
        let mut turn = 0;
        let _mock = test_support::install(move |_messages, _tools| {
            turn += 1;
            match turn {
                1 => Ok(two_calls(
                    ("c1", "write", json!({"path":"a.txt","content":"a"})),
                    ("c2", "write", json!({"path":"b.txt","content":"b"})),
                )),
                _ => Ok(text("done")),
            }
        });

        let token = CancellationToken::new();
        let cancel = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel.cancel();
        });

        let (tx, rx) = mpsc::unbounded_channel();
        let (_approval_tx, approval_rx) = mpsc::unbounded_channel();
        run(
            config.clone(),
            "cancelbatch".into(),
            "write two".into(),
            json!({}),
            false,
            tx,
            approval_rx,
            token,
        )
        .await
        .unwrap();

        let events = drain(rx);
        assert!(events
            .iter()
            .any(|event| event["type"] == "error" && event["text"] == "cancelled"));
        assert!(!config.project_dir.join("a.txt").exists());
        assert!(!config.project_dir.join("b.txt").exists());

        let messages = history(&config, "cancelbatch");
        let replies: Vec<_> = messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .collect();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["tool_call_id"], "c1");
        assert_eq!(replies[1]["tool_call_id"], "c2");
        for reply in replies {
            assert!(reply["content"].as_str().unwrap().contains("cancelled"));
        }
        strict_history(&config, "cancelbatch");
    }
}
