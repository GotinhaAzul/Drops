//! Bounded, session-scoped task memory.
//!
//! Memory belongs to one Drop session and is stored as append-only `memory`
//! records in the same JSONL log as the conversation. The latest valid record
//! wins, so a dropped or restarted session reloads the same memory without an
//! extra summarization request. A copied/forked session carries its memory;
//! clearing or removing the session removes it.
//!
//! The model updates memory through the small `task_memory` tool. Entries are
//! evidence, never authorization: paths are confined to the current tool
//! workspace and `verified` entries must cite evidence. When the workspace
//! root or anchor changes, path-scoped and anchor-scoped entries are removed
//! instead of silently remaining valid in a different context.

use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::paths;
use crate::session::SessionLog;

const MAX_GOAL_CHARS: usize = 500;
const MAX_ITEM_CHARS: usize = 240;
const MAX_EVIDENCE_CHARS: usize = 240;
const MAX_ITEMS: usize = 8;
const MAX_PATHS: usize = 8;
const MAX_RENDER_CHARS: usize = 4_000;
const MAX_INVALIDATION_CHARS: usize = 400;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct MemoryItem {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub evidence: String,
    /// Window-content observations become stale when the Drop moves.
    #[serde(default)]
    pub anchor_scoped: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct TaskMemory {
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub decisions: Vec<MemoryItem>,
    #[serde(default)]
    pub pending: Vec<MemoryItem>,
    #[serde(default)]
    pub verified: Vec<MemoryItem>,
    #[serde(default)]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub anchor: Option<String>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub invalidated: String,
}

impl TaskMemory {
    fn is_empty(&self) -> bool {
        self.goal.trim().is_empty()
            && self.decisions.is_empty()
            && self.pending.is_empty()
            && self.verified.is_empty()
            && self.invalidated.trim().is_empty()
    }
}

pub(crate) struct MemoryStore {
    log: SessionLog,
    state: Mutex<TaskMemory>,
}

impl MemoryStore {
    pub(crate) fn load(log: SessionLog) -> Self {
        let state = read_latest(log.path()).unwrap_or_default();
        Self {
            log,
            state: Mutex::new(state),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TaskMemory> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Drop entries that no longer belong to the current workspace or anchor.
    ///
    /// Returns true when memory changed. Only sessions that already contain
    /// memory write a reconciliation record; a brand-new empty session does not
    /// grow a memory record merely because it was opened.
    pub(crate) fn observe(&self, root: &Path, context: &Value) -> bool {
        let root_text = root.to_string_lossy().into_owned();
        let anchor = anchor_signature(context);
        let mut state = self.lock();
        let had_memory = !state.is_empty() || state.workspace_root.is_some();
        let mut notes: Vec<String> = Vec::new();

        if let Some(previous) = state.workspace_root.clone() {
            if previous != root_text {
                let removed = remove_path_scoped(&mut state);
                notes.push(format!(
                    "Workspace changed from `{previous}` to `{root_text}`; removed {removed} path-scoped item(s)."
                ));
            }
        }
        if let Some(previous) = state.anchor.clone() {
            if Some(&previous) != anchor.as_ref() {
                let current = anchor.as_deref().unwrap_or("unknown anchor");
                let removed = remove_anchor_scoped(&mut state);
                notes.push(format!(
                    "Anchor changed from {previous} to {current}; removed {removed} anchor-scoped item(s)."
                ));
            }
        }

        let changed =
            state.workspace_root.as_deref() != Some(root_text.as_str()) || state.anchor != anchor;
        state.workspace_root = Some(root_text);
        state.anchor = anchor;
        if !notes.is_empty() {
            state.invalidated = truncate_chars(notes.join(" "), MAX_INVALIDATION_CHARS);
        }
        if !(changed && had_memory) {
            return false;
        }
        state.revision = state.revision.saturating_add(1);
        let event = json!({
            "type": "memory",
            "timestamp": now_millis(),
            "revision": state.revision,
            "memory": &*state,
        });
        if let Err(err) = self.log.append_value(&event) {
            // Memory is advisory. A failed append must not abort the turn; the
            // in-process state remains usable for the rest of this run.
            eprintln!("task memory save failed: {err:#}");
        }
        true
    }

    /// System-prompt section for the current model turn, or `None` when no
    /// memory has been recorded yet.
    pub(crate) fn system_section(&self) -> Option<String> {
        let state = self.lock();
        if state.is_empty() {
            return None;
        }
        let mut out = String::from(
            "## Session task memory\n\
             This is bounded working memory for this Drop, not authorization and not newer than the \
             current context brief. Re-check paths and re-read files before acting. Update it with the \
             `task_memory` tool when the goal, decisions, pending work or verified results change.\n",
        );
        if !state.goal.trim().is_empty() {
            out.push_str(&format!("\nGoal: {}", state.goal));
        }
        render_items(&mut out, "Decisions", &state.decisions, false);
        render_items(&mut out, "Pending", &state.pending, false);
        render_items(&mut out, "Verified", &state.verified, true);
        if !state.invalidated.trim().is_empty() {
            out.push_str(&format!(
                "\nInvalidated earlier in this session: {}",
                state.invalidated
            ));
        }
        Some(truncate_chars(out, MAX_RENDER_CHARS))
    }

    /// Execute the `task_memory` tool. Session memory is read-only with respect
    /// to the workspace and needs no approval.
    pub(crate) fn execute(&self, arguments: &Value, root: &Path) -> Result<String, String> {
        let action = arguments
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing string argument `action`".to_string())?;
        match action {
            "get" => Ok(self.render_result()),
            "clear" => {
                let mut guard = self.lock();
                let mut state = guard.clone();
                state.goal.clear();
                state.decisions.clear();
                state.pending.clear();
                state.verified.clear();
                state.invalidated.clear();
                state.revision = state.revision.saturating_add(1);
                self.persist(&state)?;
                *guard = state;
                Ok("Session task memory cleared.".to_string())
            }
            "update" => {
                let mut guard = self.lock();
                let mut updated = guard.clone();
                if let Some(goal) = arguments.get("goal") {
                    let goal = goal
                        .as_str()
                        .ok_or_else(|| "`goal` must be a string".to_string())?;
                    updated.goal = bounded_text(goal, "goal", MAX_GOAL_CHARS)?;
                }
                if let Some(items) = arguments.get("decisions") {
                    updated.decisions = parse_items(items, "decisions", root, false)?;
                }
                if let Some(items) = arguments.get("pending") {
                    updated.pending = parse_items(items, "pending", root, false)?;
                }
                if let Some(items) = arguments.get("verified") {
                    updated.verified = parse_items(items, "verified", root, true)?;
                }
                // A successful update supersedes the previous invalidation note.
                updated.invalidated.clear();
                updated.revision = updated.revision.saturating_add(1);
                self.persist(&updated)?;
                *guard = updated;
                drop(guard);
                Ok(self.render_result())
            }
            other => Err(format!(
                "unknown action `{other}`; use `get`, `update` or `clear`"
            )),
        }
    }

    fn persist(&self, state: &TaskMemory) -> Result<(), String> {
        let event = json!({
            "type": "memory",
            "timestamp": now_millis(),
            "revision": state.revision,
            "memory": state,
        });
        self.log
            .append_value(&event)
            .map_err(|err| format!("saving task memory: {err:#}"))
    }

    fn render_result(&self) -> String {
        let state = self.lock();
        if state.is_empty() {
            return "Session task memory is empty.".to_string();
        }
        let mut out = format!("Session task memory (revision {}).", state.revision);
        if !state.goal.trim().is_empty() {
            out.push_str(&format!("\nGoal: {}", state.goal));
        }
        out.push_str(&format!(
            "\nDecisions: {} · Pending: {} · Verified: {}",
            state.decisions.len(),
            state.pending.len(),
            state.verified.len()
        ));
        if !state.invalidated.trim().is_empty() {
            out.push_str(&format!("\nInvalidated: {}", state.invalidated));
        }
        out.push_str(
            "\nThis memory is session state, not permission to act. Verify before claiming success.",
        );
        truncate_chars(out, MAX_RENDER_CHARS)
    }
}

fn render_items(out: &mut String, label: &str, items: &[MemoryItem], evidence: bool) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("\n{label}:"));
    for item in items {
        out.push_str("\n- ");
        out.push_str(&item.text);
        if evidence && !item.evidence.is_empty() {
            out.push_str(&format!(" [evidence: {}]", item.evidence));
        }
        if !item.paths.is_empty() {
            out.push_str(&format!(" [paths: {}]", item.paths.join(", ")));
        }
        if item.anchor_scoped {
            out.push_str(" [anchor-scoped]");
        }
    }
}

fn parse_items(
    value: &Value,
    field: &str,
    root: &Path,
    require_evidence: bool,
) -> Result<Vec<MemoryItem>, String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("`{field}` must be an array"))?;
    if items.len() > MAX_ITEMS {
        return Err(format!("`{field}` may contain at most {MAX_ITEMS} items"));
    }
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let object = item
                .as_object()
                .ok_or_else(|| format!("{field}[{index}] must be an object"))?;
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{field}[{index}] is missing string `text`"))?;
            let text = bounded_text(text, &format!("{field}[{index}].text"), MAX_ITEM_CHARS)?;
            if text.trim().is_empty() {
                return Err(format!("{field}[{index}].text must not be empty"));
            }
            let evidence = object
                .get("evidence")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let evidence = bounded_text(
                evidence,
                &format!("{field}[{index}].evidence"),
                MAX_EVIDENCE_CHARS,
            )?;
            if require_evidence && evidence.trim().is_empty() {
                return Err(format!(
                    "{field}[{index}] needs `evidence` from a tool result or the conversation"
                ));
            }
            let mut paths = Vec::new();
            if let Some(values) = object.get("paths") {
                let values = values
                    .as_array()
                    .ok_or_else(|| format!("{field}[{index}].paths must be an array"))?;
                if values.len() > MAX_PATHS {
                    return Err(format!(
                        "{field}[{index}].paths may contain at most {MAX_PATHS} paths"
                    ));
                }
                for path in values {
                    let path = path
                        .as_str()
                        .ok_or_else(|| format!("{field}[{index}].paths entries must be strings"))?;
                    paths.push(normalize_path(root, path)?);
                }
            }
            let anchor_scoped = object
                .get("anchor_scoped")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(MemoryItem {
                text,
                paths,
                evidence,
                anchor_scoped,
            })
        })
        .collect()
}

fn normalize_path(root: &Path, path: &str) -> Result<String, String> {
    let resolved = paths::confine(root, path).map_err(|err| format!("path `{path}`: {err:#}"))?;
    let relative = resolved
        .strip_prefix(root)
        .map_err(|_| format!("path `{path}` is outside the tool workspace"))?;
    let text = relative.to_string_lossy().into_owned();
    if text.is_empty() {
        return Err("path must name a file or directory inside the workspace".to_string());
    }
    if text.chars().count() > 300 {
        return Err("stored paths must be at most 300 characters".to_string());
    }
    Ok(text)
}

fn bounded_text(value: &str, field: &str, max: usize) -> Result<String, String> {
    let value = value.trim();
    if value.chars().count() > max {
        return Err(format!(
            "`{field}` is longer than {max} characters; keep memory compact"
        ));
    }
    Ok(value.to_string())
}

fn remove_path_scoped(state: &mut TaskMemory) -> usize {
    let mut removed = 0;
    for items in [
        &mut state.decisions,
        &mut state.pending,
        &mut state.verified,
    ] {
        let before = items.len();
        items.retain(|item| item.paths.is_empty());
        removed += before - items.len();
    }
    removed
}

fn remove_anchor_scoped(state: &mut TaskMemory) -> usize {
    let mut removed = 0;
    for items in [
        &mut state.decisions,
        &mut state.pending,
        &mut state.verified,
    ] {
        let before = items.len();
        items.retain(|item| !item.anchor_scoped);
        removed += before - items.len();
    }
    removed
}

fn anchor_signature(context: &Value) -> Option<String> {
    let kind = context.get("kind").and_then(Value::as_str)?;
    match kind {
        "window" => {
            let address = context
                .pointer("/window/address")
                .and_then(Value::as_str)
                .unwrap_or("");
            let pid = context
                .pointer("/window/pid")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            Some(format!("window:{address}:{pid}"))
        }
        "workspace" => {
            let id = context
                .pointer("/workspace/id")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            Some(format!("workspace:{id}"))
        }
        _ => Some(format!("mode:{kind}")),
    }
}

fn read_latest(path: &Path) -> Option<TaskMemory> {
    use std::io::{BufRead, BufReader, Read};
    const MAX_RECORD_BYTES: u64 = 512 * 1024;
    let mut reader = BufReader::new(std::fs::File::open(path).ok()?);
    let mut latest = None;
    loop {
        let mut line = Vec::new();
        let read = (&mut reader)
            .take(MAX_RECORD_BYTES + 1)
            .read_until(b'\n', &mut line)
            .ok()?;
        if read == 0 {
            break;
        }
        if line.len() as u64 > MAX_RECORD_BYTES {
            if line.last() != Some(&b'\n') {
                reader.skip_until(b'\n').ok()?;
            }
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("memory") {
            continue;
        }
        let Some(memory) = value.get("memory") else {
            continue;
        };
        if let Ok(memory) = serde_json::from_value::<TaskMemory>(memory.clone()) {
            if valid_loaded_memory(&memory) {
                latest = Some(memory);
            }
        }
    }
    latest
}

// Exported sessions may be manually edited or truncated. Re-apply size limits
// when loading instead of assuming every JSONL memory record came from a tool.
fn valid_loaded_memory(memory: &TaskMemory) -> bool {
    if memory.goal.chars().count() > MAX_GOAL_CHARS
        || memory.invalidated.chars().count() > MAX_INVALIDATION_CHARS
        || memory
            .workspace_root
            .as_ref()
            .is_some_and(|root| root.len() > 4096)
        || memory
            .anchor
            .as_ref()
            .is_some_and(|anchor| anchor.len() > 512)
    {
        return false;
    }
    for (items, verified) in [
        (&memory.decisions, false),
        (&memory.pending, false),
        (&memory.verified, true),
    ] {
        if items.len() > MAX_ITEMS {
            return false;
        }
        for item in items {
            if item.text.trim().is_empty()
                || item.text.chars().count() > MAX_ITEM_CHARS
                || item.evidence.chars().count() > MAX_EVIDENCE_CHARS
                || (verified && item.evidence.trim().is_empty())
                || item.paths.len() > MAX_PATHS
                || item.paths.iter().any(|path| path.chars().count() > 300)
            {
                return false;
            }
        }
    }
    true
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn truncate_chars(mut text: String, max: usize) -> String {
    if text.chars().count() <= max {
        return text;
    }
    text = text.chars().take(max).collect();
    text.push_str("\n...[memory truncated]");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::path::PathBuf;

    fn store() -> (TempDir, PathBuf, MemoryStore) {
        let temp = TempDir::new();
        let root = paths::canonical_root(&temp.path().join("project")).unwrap();
        let log = SessionLog::new(temp.path().join("s.jsonl"));
        log.ensure_header("s", &root).unwrap();
        (temp, root, MemoryStore::load(log))
    }

    fn window(anchor: &str) -> Value {
        json!({"kind":"window","window":{"address":anchor}})
    }

    #[test]
    fn memory_round_trips_through_the_session_log() {
        let (_temp, root, memory) = store();
        let result = memory
            .execute(
                &json!({
                    "action":"update",
                    "goal":"Ship the context brief",
                    "decisions":[{"text":"Use the existing inspector","paths":["src/lib.rs"]}],
                    "pending":[{"text":"Add regression tests","paths":["tests"]}],
                    "verified":[{"text":"Workspace tests passed","evidence":"cargo test --workspace"}]
                }),
                &root,
            )
            .unwrap();
        assert!(result.contains("Ship the context brief"));

        let reloaded = MemoryStore::load(SessionLog::new(memory.log.path().to_path_buf()));
        let section = reloaded.system_section().unwrap();
        assert!(section.contains("Ship the context brief"));
        assert!(section.contains("[evidence: cargo test --workspace]"));
        assert!(section.contains("[paths: src/lib.rs]"));
    }

    #[test]
    fn reload_skips_oversized_and_invalid_records_but_keeps_latest_valid_memory() {
        use std::io::Write;
        let (_temp, root, memory) = store();
        memory
            .execute(&json!({"action":"update", "goal":"Valid"}), &root)
            .unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(memory.log.path())
            .unwrap();
        file.write_all(&vec![b'x'; 600_000]).unwrap();
        file.write_all(b"\n").unwrap();
        memory
            .log
            .append_value(&json!({"type":"memory", "memory":{"goal":"x".repeat(501)}}))
            .unwrap();
        let loaded = MemoryStore::load(memory.log.clone());
        assert!(loaded.system_section().unwrap().contains("Valid"));
        memory
            .execute(&json!({"action":"update", "goal":"Later valid"}), &root)
            .unwrap();
        let loaded = MemoryStore::load(memory.log.clone());
        assert!(loaded.system_section().unwrap().contains("Later valid"));
    }

    #[test]
    fn rejected_memory_update_is_transactional() {
        let (_temp, root, memory) = store();
        memory
            .execute(&json!({"action":"update", "goal":"Original"}), &root)
            .unwrap();
        assert!(memory.execute(&json!({"action":"update", "goal":"Partial mutation", "verified":[{"text":"No evidence"}]}), &root).is_err());
        assert!(memory.system_section().unwrap().contains("Original"));
        assert!(!memory
            .system_section()
            .unwrap()
            .contains("Partial mutation"));
    }

    #[test]
    fn failed_persistence_does_not_change_in_process_memory() {
        let (_temp, root, memory) = store();
        memory
            .execute(&json!({"action":"update", "goal":"Original"}), &root)
            .unwrap();
        std::fs::remove_file(memory.log.path()).unwrap();
        std::fs::create_dir(memory.log.path()).unwrap();
        assert!(memory
            .execute(&json!({"action":"update", "goal":"Unsaved"}), &root)
            .is_err());
        assert!(memory.system_section().unwrap().contains("Original"));
        assert!(memory.execute(&json!({"action":"clear"}), &root).is_err());
        assert!(memory.system_section().unwrap().contains("Original"));
    }

    #[test]
    fn losing_anchor_evidence_invalidates_anchor_scoped_memory() {
        let (_temp, root, memory) = store();
        memory.observe(&root, &window("0x1"));
        memory.execute(&json!({"action":"update", "pending":[{"text":"Old window observation", "anchor_scoped":true}]}), &root).unwrap();
        memory.observe(&root, &json!({"mode":"headless"}));
        assert!(!memory
            .system_section()
            .unwrap()
            .contains("Old window observation"));
    }

    #[test]
    fn verified_items_require_evidence_and_paths_stay_in_workspace() {
        let (_temp, root, memory) = store();
        let err = memory
            .execute(
                &json!({"action":"update","verified":[{"text":"It works"}]}),
                &root,
            )
            .unwrap_err();
        assert!(err.contains("needs `evidence`"), "{err}");
        let err = memory
            .execute(
                &json!({"action":"update","pending":[{"text":"Write","paths":["../outside"]}]}),
                &root,
            )
            .unwrap_err();
        assert!(err.contains("outside") || err.contains("escapes"), "{err}");
        assert!(memory.system_section().is_none());
    }

    #[test]
    fn workspace_change_removes_path_scoped_items_only() {
        let (temp, root, memory) = store();
        memory.observe(&root, &window("0x1"));
        memory
            .execute(
                &json!({"action":"update","goal":"Keep the user goal",
                    "decisions":[{"text":"Path decision","paths":["a.txt"]},{"text":"General decision"}],
                    "verified":[{"text":"Test passed","evidence":"cargo test"}]}),
                &root,
            )
            .unwrap();
        let other = paths::canonical_root(&temp.path().join("other")).unwrap();
        assert!(memory.observe(&other, &window("0x1")));
        let section = memory.system_section().unwrap();
        assert!(section.contains("Keep the user goal"));
        assert!(section.contains("General decision"));
        assert!(!section.contains("Path decision"));
        assert!(section.contains("Test passed"));
        assert!(section.contains("Workspace changed"));
    }

    #[test]
    fn anchor_change_removes_anchor_scoped_items() {
        let (_temp, root, memory) = store();
        memory.observe(&root, &window("0x1"));
        memory
            .execute(
                &json!({"action":"update",
                    "pending":[{"text":"Window observation","anchor_scoped":true},
                               {"text":"Stable task"}]}),
                &root,
            )
            .unwrap();
        assert!(memory.observe(&root, &window("0x2")));
        let section = memory.system_section().unwrap();
        assert!(!section.contains("Window observation"));
        assert!(section.contains("Stable task"));
    }

    #[test]
    fn clear_and_empty_update_are_deliberate() {
        let (_temp, root, memory) = store();
        memory.observe(&root, &window("0x1"));
        memory
            .execute(&json!({"action":"update","goal":"Temporary"}), &root)
            .unwrap();
        assert!(memory.system_section().unwrap().contains("Temporary"));
        memory.execute(&json!({"action":"clear"}), &root).unwrap();
        assert!(memory.system_section().is_none());
        memory
            .execute(
                &json!({"action":"update","decisions":[],"pending":[],"verified":[]}),
                &root,
            )
            .unwrap();
        assert!(memory.system_section().is_none());
    }

    #[test]
    fn export_and_fork_are_session_file_operations() {
        let (_temp, root, memory) = store();
        memory
            .execute(&json!({"action":"update","goal":"Fork me"}), &root)
            .unwrap();
        let source = memory.log.path().to_path_buf();
        let copy = source.with_file_name("fork.jsonl");
        std::fs::copy(&source, &copy).unwrap();
        let forked = MemoryStore::load(SessionLog::new(copy));
        assert!(forked.system_section().unwrap().contains("Fork me"));
        std::fs::remove_file(&source).unwrap();
        let cleared = MemoryStore::load(SessionLog::new(source));
        assert!(cleared.system_section().is_none());
    }
}
