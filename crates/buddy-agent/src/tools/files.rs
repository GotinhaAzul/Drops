//! File tools. Every path goes through `paths::confine` first.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::paths;
use crate::tools::{optional_u64, string_arg, ToolContext, ToolOutcome};

const DEFAULT_READ_LINES: usize = 2_000;
const MAX_READ_LINES: usize = 5_000;
const MAX_READ_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_READ_OUTPUT_BYTES: usize = 50_000;
const MAX_WRITE_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_BACKUPS_PER_FILE: usize = 10;
const MAX_PREVIEW_LINES: usize = 120;
const MAX_PREVIEW_BYTES: usize = 24 * 1024;
const DIFF_CONTEXT_LINES: usize = 3;
const DIFF_TRUNCATION_MARKER: &str =
    "... [diff truncated at 24 KiB; inspect the omitted changes before approving]\n";
const LCS_MAX_CELLS: usize = 1_000_000;
const LCS_MAX_ROWS: usize = 20_000;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Fingerprints of files read during this run. A write/edit/append refuses to
/// replace a file that changed after the model read it, including while an
/// approval prompt was waiting. Fingerprints are only an in-process safety net;
/// `expected_content` remains the explicit cross-session check.
///
/// Previews also freeze a separate snapshot keyed by the requested path. The
/// snapshot records the resolved target, whether it existed, and its bytes, so
/// execution cannot silently apply an approved change to a file that appeared,
/// vanished, changed, or was reached through a retargeted symlink while the
/// approval prompt was waiting. This does not close the race between the final
/// execution check and the rename; it closes the approval-delay window.
#[derive(Default)]
pub(crate) struct WriteTracker {
    receipts: Mutex<HashMap<PathBuf, Fingerprint>>,
    prepared: Mutex<HashMap<String, PreparedSnapshot>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fingerprint {
    len: u64,
    hash: u64,
}

#[derive(Clone, Debug)]
struct PreparedSnapshot {
    target: PathBuf,
    exists: bool,
    fingerprint: Fingerprint,
}

impl WriteTracker {
    fn record(&self, path: &Path, bytes: &[u8]) {
        let mut receipts = self
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Bound the map: file tools are per-turn, but a model can read many
        // paths. Keeping the most recent 256 is enough for stale-read checks.
        if receipts.len() >= 256 && !receipts.contains_key(path) {
            receipts.clear();
        }
        receipts.insert(path.to_path_buf(), fingerprint(bytes));
    }

    fn changed_since_read(&self, path: &Path, bytes: &[u8]) -> bool {
        let receipts = self
            .receipts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        receipts
            .get(path)
            .is_some_and(|previous| *previous != fingerprint(bytes))
    }

    /// Freeze the exact target an approval card described. This is in-memory
    /// only; a preview must never create the file, a parent, or a backup.
    fn prepare(&self, requested: &str, target: &Path, existing: &ExistingFile) {
        let mut prepared = self
            .prepared
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if prepared.len() >= 64 && !prepared.contains_key(requested) {
            prepared.clear();
        }
        prepared.insert(
            requested.to_string(),
            PreparedSnapshot {
                target: target.to_path_buf(),
                exists: existing.exists,
                fingerprint: fingerprint(&existing.bytes),
            },
        );
    }

    /// Consume a prepared snapshot, if any, and require the target to still be
    /// the same file in the same state. Direct tool execution without a preview
    /// has no snapshot and keeps working; independent since-read checks remain.
    fn verify_prepared(
        &self,
        requested: &str,
        target: &Path,
        existing: &ExistingFile,
        display: &str,
    ) -> Result<(), String> {
        let snapshot = {
            let mut prepared = self
                .prepared
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            prepared.remove(requested)
        };
        let Some(snapshot) = snapshot else {
            return Ok(());
        };
        if snapshot.target != target {
            return Err(format!(
                "`{display}` now resolves to a different file than the approved preview (the path or a symlink changed); inspect it again. No changes made."
            ));
        }
        if snapshot.exists != existing.exists {
            return Err(if snapshot.exists {
                format!(
                    "`{display}` was deleted after the approved preview; inspect it again before editing. No changes made."
                )
            } else {
                format!(
                    "`{display}` appeared after the approved create preview; inspect it again before writing. No changes made."
                )
            });
        }
        if snapshot.exists && snapshot.fingerprint != fingerprint(&existing.bytes) {
            return Err(format!(
                "`{display}` changed after it was read for the approved preview; read it again before editing. No changes made."
            ));
        }
        Ok(())
    }
}

struct ExistingFile {
    text: String,
    bytes: Vec<u8>,
    mode: Option<u32>,
    exists: bool,
}

pub(crate) fn read(ctx: &ToolContext, arguments: &Value) -> ToolOutcome {
    let requested = match string_arg(arguments, "path") {
        Ok(path) => path,
        Err(outcome) => return outcome,
    };
    let offset = optional_u64(arguments, "offset").unwrap_or(1).max(1) as usize;
    let limit = optional_u64(arguments, "limit")
        .unwrap_or(DEFAULT_READ_LINES as u64)
        .clamp(1, MAX_READ_LINES as u64) as usize;

    let path = match paths::confine(&ctx.root, requested) {
        Ok(path) => path,
        Err(err) => return ToolOutcome::error(format!("read: {err:#}")),
    };

    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) => {
            return ToolOutcome::error(format!(
                "read: cannot access `{}`: {err}",
                paths::display_path(&ctx.root, &path)
            ))
        }
    };
    if !metadata.is_file() {
        return ToolOutcome::error(format!(
            "read: `{}` is not a regular file (directories and devices cannot be read).",
            paths::display_path(&ctx.root, &path)
        ));
    }
    if metadata.len() > MAX_READ_FILE_BYTES {
        return ToolOutcome::error(format!(
            "read: `{}` is {} bytes, larger than the {} byte limit.",
            paths::display_path(&ctx.root, &path),
            metadata.len(),
            MAX_READ_FILE_BYTES
        ));
    }

    let bytes = match paths::read_bounded(&path, MAX_READ_FILE_BYTES as usize) {
        Ok(bytes) => bytes,
        Err(err) => {
            return ToolOutcome::error(format!(
                "read: cannot read `{}`: {err}",
                paths::display_path(&ctx.root, &path)
            ))
        }
    };
    if bytes.len() as u64 > MAX_READ_FILE_BYTES {
        return ToolOutcome::error("read: file grew beyond the 8 MiB limit; narrow the input.");
    }
    ctx.writes.record(&path, &bytes);
    if bytes.contains(&0) {
        return ToolOutcome::error(format!(
            "read: `{}` looks like a binary file.",
            paths::display_path(&ctx.root, &path)
        ));
    }

    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(_) => return ToolOutcome::error("read: file is not valid UTF-8 text."),
    };
    if text.is_empty() {
        return ToolOutcome::ok("(empty file)");
    }

    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if offset > total {
        return ToolOutcome::error(format!(
            "read: offset {offset} is beyond the end of `{}` ({total} lines).",
            paths::display_path(&ctx.root, &path)
        ));
    }

    let start = offset - 1;
    let wanted_end = start.saturating_add(limit).min(total);
    let mut out = String::new();
    let mut end = start;
    while end < wanted_end {
        let line = lines[end];
        let added = if out.is_empty() {
            line.len()
        } else {
            line.len() + 1
        };
        if !out.is_empty() && out.len() + added > MAX_READ_OUTPUT_BYTES {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        if out.len() < 64 && line.len() > MAX_READ_OUTPUT_BYTES {
            // A single enormous line: show a bounded prefix.
            let mut prefix = line.to_string();
            let mut boundary = MAX_READ_OUTPUT_BYTES;
            while !prefix.is_char_boundary(boundary) {
                boundary -= 1;
            }
            prefix.truncate(boundary);
            out.push_str(&prefix);
            out.push_str("...[line truncated]");
            end += 1;
            break;
        }
        out.push_str(line);
        end += 1;
    }

    if out.len() > MAX_READ_OUTPUT_BYTES {
        let mut cut = MAX_READ_OUTPUT_BYTES;
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("...[output truncated]");
    }

    if end < total {
        out.push_str(&format!(
            "\n\n[Showing lines {}-{} of {}. Use offset={} to continue.]",
            start + 1,
            end,
            total,
            end + 1
        ));
    }
    ToolOutcome::ok(out)
}

pub(crate) fn write(ctx: &ToolContext, arguments: &Value) -> ToolOutcome {
    let requested = match string_arg(arguments, "path") {
        Ok(path) => path,
        Err(outcome) => return outcome,
    };
    let content = match string_arg(arguments, "content") {
        Ok(content) => content,
        Err(outcome) => return outcome,
    };
    if content.len() as u64 > MAX_WRITE_FILE_BYTES {
        return ToolOutcome::error(format!(
            "write: result would exceed the {MAX_WRITE_FILE_BYTES} byte edit limit; use a smaller file or another approved tool."
        ));
    }

    let path = match paths::confine(&ctx.root, requested) {
        Ok(path) => path,
        Err(err) => return ToolOutcome::error(format!("write: {err:#}")),
    };
    let display = paths::display_path(&ctx.root, &path);
    let existing = match existing_text(&path, &display) {
        Ok(existing) => existing,
        Err(message) => return ToolOutcome::error(format!("write: {message}")),
    };
    if let Err(message) = ctx
        .writes
        .verify_prepared(requested, &path, &existing, &display)
    {
        return ToolOutcome::error(format!("write: {message}"));
    }
    if let Err(message) = check_expected(arguments, &existing, &display) {
        return ToolOutcome::error(format!("write: {message}"));
    }
    if existing.exists && ctx.writes.changed_since_read(&path, &existing.bytes) {
        return ToolOutcome::error(format!(
            "write: `{display}` changed after it was read; read it again before overwriting. No changes made."
        ));
    }
    if existing.exists && existing.bytes == content.as_bytes() {
        return ToolOutcome::ok(format!(
            "`{display}` already has the requested content; no write needed."
        ));
    }

    let original = existing.exists.then_some(existing.bytes.as_slice());
    match write_atomic(ctx, &path, content.as_bytes(), existing.mode, original) {
        Ok(backup) => ToolOutcome::ok(format!(
            "Successfully wrote {} bytes to `{}`.{}",
            content.len(),
            display,
            backup_suffix(ctx, backup.as_deref())
        )),
        Err(message) => ToolOutcome::error(format!("write: {message}")),
    }
}

pub(crate) fn append(ctx: &ToolContext, arguments: &Value) -> ToolOutcome {
    let requested = match string_arg(arguments, "path") {
        Ok(path) => path,
        Err(outcome) => return outcome,
    };
    let content = match string_arg(arguments, "content") {
        Ok(content) => content,
        Err(outcome) => return outcome,
    };
    let path = match paths::confine(&ctx.root, requested) {
        Ok(path) => path,
        Err(err) => return ToolOutcome::error(format!("append: {err:#}")),
    };
    let display = paths::display_path(&ctx.root, &path);
    let existing = match existing_text(&path, &display) {
        Ok(existing) => existing,
        Err(message) => return ToolOutcome::error(format!("append: {message}")),
    };
    if let Err(message) = ctx
        .writes
        .verify_prepared(requested, &path, &existing, &display)
    {
        return ToolOutcome::error(format!("append: {message}"));
    }
    if let Err(message) = check_expected(arguments, &existing, &display) {
        return ToolOutcome::error(format!("append: {message}"));
    }
    if existing.exists && ctx.writes.changed_since_read(&path, &existing.bytes) {
        return ToolOutcome::error(format!(
            "append: `{display}` changed after it was read; read it again before appending. No changes made."
        ));
    }
    let mut updated = existing.bytes.clone();
    updated.extend_from_slice(content.as_bytes());
    if updated.len() as u64 > MAX_WRITE_FILE_BYTES {
        return ToolOutcome::error(format!(
            "append: result would exceed the {MAX_WRITE_FILE_BYTES} byte edit limit; use a smaller file or another approved tool."
        ));
    }

    let original = existing.exists.then_some(existing.bytes.as_slice());
    match write_atomic(ctx, &path, &updated, existing.mode, original) {
        Ok(backup) => ToolOutcome::ok(format!(
            "Appended {} bytes to `{}`.{}",
            content.len(),
            display,
            backup_suffix(ctx, backup.as_deref())
        )),
        Err(message) => ToolOutcome::error(format!("append: {message}")),
    }
}

/// Read an existing regular text file, or return an empty representation for a
/// missing target. Devices, directories, oversized files and binary/non-UTF-8
/// data are refused before a mutation can overwrite them.
fn existing_text(path: &Path, display: &str) -> Result<ExistingFile, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ExistingFile {
                text: String::new(),
                bytes: Vec::new(),
                mode: None,
                exists: false,
            })
        }
        Err(err) => return Err(format!("cannot inspect `{display}`: {err}")),
    };
    if !metadata.is_file() {
        return Err(format!("`{display}` is not a regular file."));
    }
    if metadata.len() > MAX_WRITE_FILE_BYTES {
        return Err(format!(
            "`{display}` is larger than the {MAX_WRITE_FILE_BYTES} byte edit limit."
        ));
    }
    let bytes = paths::read_bounded(path, MAX_WRITE_FILE_BYTES as usize)
        .map_err(|err| format!("cannot read `{display}`: {err}"))?;
    if bytes.len() as u64 > MAX_WRITE_FILE_BYTES {
        return Err(format!(
            "`{display}` grew beyond the edit limit; narrow the input."
        ));
    }
    if bytes.contains(&0) {
        return Err(format!(
            "`{display}` looks like a binary file; use the appropriate document tool or leave it unchanged."
        ));
    }
    let text = String::from_utf8(bytes.clone())
        .map_err(|_| format!("`{display}` is not valid UTF-8 text."))?;
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode())
    };
    #[cfg(not(unix))]
    let mode = None;
    Ok(ExistingFile {
        text,
        bytes,
        mode,
        exists: true,
    })
}

fn check_expected(arguments: &Value, existing: &ExistingFile, display: &str) -> Result<(), String> {
    let Some(expected) = arguments.get("expected_content") else {
        return Ok(());
    };
    let expected = expected
        .as_str()
        .ok_or_else(|| "`expected_content` must be a string".to_string())?;
    if expected != existing.text {
        return Err(format!(
            "`{display}` changed since it was read (expected_content did not match); read it again before editing. No changes made."
        ));
    }
    Ok(())
}

fn backup_suffix(ctx: &ToolContext, backup: Option<&Path>) -> String {
    match backup {
        Some(path) => format!(
            " Original content backed up to `{}`.",
            paths::display_path(&ctx.root, path)
        ),
        None => String::new(),
    }
}

/// Atomically replace `path`, first writing the already-validated original
/// bytes into the workspace's private backup area. The temporary file lives in
/// the same directory, so the final rename is atomic on one filesystem.
/// Existing permission bits are used for both the temporary file and the backup.
fn write_atomic(
    ctx: &ToolContext,
    path: &Path,
    bytes: &[u8],
    mode: Option<u32>,
    original: Option<&[u8]>,
) -> Result<Option<PathBuf>, String> {
    let path = ensure_parent(&ctx.root, path)?;
    let backup = match original {
        Some(original) => Some(backup_file(&ctx.root, &path, original, mode)?),
        None => None,
    };
    let parent = path
        .parent()
        .ok_or_else(|| "refusing to write the project root itself".to_string())?;
    let name = path
        .file_name()
        .ok_or_else(|| "path has no file name".to_string())?;
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(
        ".{}.drops-tmp-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        counter
    ));

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(mode))?;
        }
        drop(file);
        std::fs::rename(&temp, &path)?;
        // Best effort: make the rename durable without turning a successful
        // write into an error if the directory cannot be synced.
        if let Ok(directory) = std::fs::File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if let Err(err) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(err.to_string());
    }
    ctx.writes.record(&path, bytes);
    Ok(backup)
}

fn backup_file(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    mode: Option<u32>,
) -> Result<PathBuf, String> {
    // Resolve the backup directory through the same confinement as every other
    // workspace path *before* creating it. This rejects a static `.drops`
    // symlink that points outside the root instead of creating directories
    // there; the canonical check after creation narrows the remaining window.
    let dir = paths::confine(root, ".drops/backups")
        .map_err(|err| format!("cannot resolve backup directory: {err:#}"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("cannot create backup directory `{}`: {err}", dir.display()))?;
    let canonical = std::fs::canonicalize(&dir)
        .map_err(|err| format!("cannot resolve backup directory `{}`: {err}", dir.display()))?;
    if !canonical.starts_with(root) {
        return Err("backup directory escapes the project root; refusing the change".to_string());
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "target is outside the project root".to_string())?;
    // A stable identity derived from the full relative path prevents flattened
    // names (`a/b.txt` vs `a_b.txt`) and prefix names (`a` vs `a.b`) from
    // sharing backup history, both when creating and when pruning.
    let identity = backup_identity(relative);
    let label = backup_label(relative);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);

    let mut suffix = 0u32;
    let (backup, write_result) = loop {
        let name = if suffix == 0 {
            format!("{identity}-{label}.{stamp}.bak")
        } else {
            format!("{identity}-{label}.{stamp}.{suffix}.bak")
        };
        let candidate = canonical.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Private by default; source permissions are applied below so a
            // private original never gets a transient world-readable backup.
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(mut file) => {
                let result = (|| -> std::io::Result<()> {
                    file.write_all(bytes)?;
                    #[cfg(unix)]
                    if let Some(mode) = mode {
                        use std::os::unix::fs::PermissionsExt;
                        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
                    }
                    file.sync_all()?;
                    Ok(())
                })();
                break (candidate, result);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && suffix < 100 => {
                suffix += 1;
            }
            Err(err) => {
                return Err(format!(
                    "cannot create backup `{}`: {err}",
                    candidate.display()
                ))
            }
        }
    };
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&backup);
        return Err(format!("cannot back up `{}`: {err}", path.display()));
    }
    prune_backups(&canonical, &identity);
    Ok(backup)
}

fn backup_identity(relative: &Path) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in relative.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn backup_label(relative: &Path) -> String {
    let mut label: String = relative
        .to_string_lossy()
        .chars()
        .map(|ch| match ch {
            '/' | '\\' => '_',
            ch if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') => ch,
            _ => '_',
        })
        .collect();
    if label.chars().count() > 80 {
        label = label.chars().take(80).collect();
    }
    if label.is_empty() {
        "file".to_string()
    } else {
        label
    }
}

fn prune_backups(dir: &Path, identity: &str) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = format!("{identity}-");
    let mut backups: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            (name.starts_with(&prefix) && name.ends_with(".bak")).then(|| entry.path())
        })
        .collect();
    if backups.len() <= MAX_BACKUPS_PER_FILE {
        return;
    }
    backups.sort_by_key(|path| {
        let modified = std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        (modified, path.file_name().map(|name| name.to_owned()))
    });
    let remove = backups.len() - MAX_BACKUPS_PER_FILE;
    for path in backups.into_iter().take(remove) {
        let _ = std::fs::remove_file(path);
    }
}

/// Create parents, then re-verify the canonical parent is still inside root.
fn ensure_parent(root: &Path, path: &Path) -> Result<std::path::PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "refusing to write the project root itself".to_string())?;
    let name = path
        .file_name()
        .ok_or_else(|| "path has no file name".to_string())?;

    std::fs::create_dir_all(parent).map_err(|err| {
        format!(
            "failed to create parent directory `{}`: {err}",
            parent.display()
        )
    })?;
    let canonical_parent = std::fs::canonicalize(parent).map_err(|err| {
        format!(
            "failed to resolve parent directory `{}`: {err}",
            parent.display()
        )
    })?;
    if !canonical_parent.starts_with(root) {
        return Err("path escapes project root".to_string());
    }
    Ok(canonical_parent.join(name))
}

pub(crate) fn edit(ctx: &ToolContext, arguments: &Value) -> ToolOutcome {
    let requested = match string_arg(arguments, "path") {
        Ok(path) => path,
        Err(outcome) => return outcome,
    };
    let (replacements, simultaneous, replace_all) = match replacements(arguments) {
        Ok(parts) => parts,
        Err(message) => return ToolOutcome::error(format!("edit: {message}")),
    };

    let path = match paths::confine(&ctx.root, requested) {
        Ok(path) => path,
        Err(err) => return ToolOutcome::error(format!("edit: {err:#}")),
    };
    let display = paths::display_path(&ctx.root, &path);
    let existing = match existing_text(&path, &display) {
        Ok(existing) if existing.exists => existing,
        Ok(_) => {
            return ToolOutcome::error(format!(
                "edit: `{display}` does not exist; use write to create it."
            ))
        }
        Err(message) => return ToolOutcome::error(format!("edit: {message}")),
    };
    if let Err(message) = ctx
        .writes
        .verify_prepared(requested, &path, &existing, &display)
    {
        return ToolOutcome::error(format!("edit: {message}"));
    }
    if let Err(message) = check_expected(arguments, &existing, &display) {
        return ToolOutcome::error(format!("edit: {message}"));
    }
    if ctx.writes.changed_since_read(&path, &existing.bytes) {
        return ToolOutcome::error(format!(
            "edit: `{display}` changed after it was read; read it again before editing. No changes made."
        ));
    }

    let (updated, count) =
        match apply_replacements(&existing.text, &replacements, simultaneous, replace_all) {
            Ok(result) => result,
            Err(message) => return ToolOutcome::error(format!("edit: {message}")),
        };
    if updated.len() as u64 > MAX_WRITE_FILE_BYTES {
        return ToolOutcome::error(format!(
            "edit: result would exceed the {MAX_WRITE_FILE_BYTES} byte edit limit; use a smaller file or another approved tool."
        ));
    }

    let original = existing.exists.then_some(existing.bytes.as_slice());
    match write_atomic(ctx, &path, updated.as_bytes(), existing.mode, original) {
        Ok(backup) => ToolOutcome::ok(format!(
            "Successfully edited `{display}` ({count} replacement{}).{}",
            if count == 1 { "" } else { "s" },
            backup_suffix(ctx, backup.as_deref())
        )),
        Err(message) => ToolOutcome::error(format!("edit: {message}")),
    }
}

pub(crate) fn preview_write(ctx: &ToolContext, arguments: &Value) -> Result<Option<Value>, String> {
    let requested = string_arg(arguments, "path").map_err(|outcome| outcome.content)?;
    let content = string_arg(arguments, "content").map_err(|outcome| outcome.content)?;
    if content.len() as u64 > MAX_WRITE_FILE_BYTES {
        return Err(format!(
            "result would exceed the {MAX_WRITE_FILE_BYTES} byte edit limit; use a smaller file or another approved tool"
        ));
    }
    let path = paths::confine(&ctx.root, requested).map_err(|err| format!("{err:#}"))?;
    let display = paths::display_path(&ctx.root, &path);
    let existing = existing_text(&path, &display)?;
    check_expected(arguments, &existing, &display)?;
    if existing.exists && ctx.writes.changed_since_read(&path, &existing.bytes) {
        return Err(format!(
            "`{display}` changed after it was read; read it again before overwriting"
        ));
    }
    ctx.writes.prepare(requested, &path, &existing);
    Ok(Some(build_preview(
        "write",
        &display,
        &existing.text,
        content,
        existing.exists,
    )))
}

pub(crate) fn preview_append(
    ctx: &ToolContext,
    arguments: &Value,
) -> Result<Option<Value>, String> {
    let requested = string_arg(arguments, "path").map_err(|outcome| outcome.content)?;
    let content = string_arg(arguments, "content").map_err(|outcome| outcome.content)?;
    let path = paths::confine(&ctx.root, requested).map_err(|err| format!("{err:#}"))?;
    let display = paths::display_path(&ctx.root, &path);
    let existing = existing_text(&path, &display)?;
    check_expected(arguments, &existing, &display)?;
    if existing.exists && ctx.writes.changed_since_read(&path, &existing.bytes) {
        return Err(format!(
            "`{display}` changed after it was read; read it again before appending"
        ));
    }
    let result_len = (existing.bytes.len() as u64).saturating_add(content.len() as u64);
    if result_len > MAX_WRITE_FILE_BYTES {
        return Err(format!(
            "result would exceed the {MAX_WRITE_FILE_BYTES} byte edit limit; use a smaller file or another approved tool"
        ));
    }
    let mut updated = existing.text.clone();
    updated.push_str(content);
    ctx.writes.prepare(requested, &path, &existing);
    Ok(Some(build_preview(
        "append",
        &display,
        &existing.text,
        &updated,
        existing.exists,
    )))
}

pub(crate) fn preview_edit(ctx: &ToolContext, arguments: &Value) -> Result<Option<Value>, String> {
    let requested = string_arg(arguments, "path").map_err(|outcome| outcome.content)?;
    let (replacements, simultaneous, replace_all) = replacements(arguments)?;
    let path = paths::confine(&ctx.root, requested).map_err(|err| format!("{err:#}"))?;
    let display = paths::display_path(&ctx.root, &path);
    let existing = existing_text(&path, &display)?;
    if !existing.exists {
        return Err(format!(
            "`{display}` does not exist; use write to create it"
        ));
    }
    check_expected(arguments, &existing, &display)?;
    if ctx.writes.changed_since_read(&path, &existing.bytes) {
        return Err(format!(
            "`{display}` changed after it was read; read it again before editing"
        ));
    }
    let (updated, _) =
        apply_replacements(&existing.text, &replacements, simultaneous, replace_all)?;
    if updated.len() as u64 > MAX_WRITE_FILE_BYTES {
        return Err(format!(
            "result would exceed the {MAX_WRITE_FILE_BYTES} byte edit limit; use a smaller file or another approved tool"
        ));
    }
    ctx.writes.prepare(requested, &path, &existing);
    Ok(Some(build_preview(
        "edit",
        &display,
        &existing.text,
        &updated,
        true,
    )))
}

fn build_preview(kind: &str, display: &str, old: &str, new: &str, existed: bool) -> Value {
    let (diff, additions, removals, newline_change) = unified_diff(display, old, new, existed);
    let summary = if additions == 0 && removals == 0 {
        match newline_change {
            Some("removed") => "Final newline removed.".to_string(),
            Some("added") => "Final newline added.".to_string(),
            _ => "No content changes.".to_string(),
        }
    } else {
        let mut summary = format!("{additions} line(s) added, {removals} line(s) removed.");
        if let Some(change) = newline_change {
            summary.push_str(&format!(" Final newline {change}."));
        }
        summary
    };
    let mut preview = json!({
        "kind": kind,
        "path": display,
        "summary": summary,
        "diff": diff,
        "existing": existed,
    });
    if existed {
        preview["recovery"] =
            json!("Original content is copied under .drops/backups before replacement.");
    }
    preview
}

#[derive(Clone, Copy)]
enum DiffOp<'a> {
    Same(&'a str),
    Remove(&'a str),
    Add(&'a str),
    /// Display-only marker; it does not consume a line on either side.
    NoNewline,
}

/// Small line-oriented diff for review. It uses LCS for ordinary files and a
/// bounded prefix/suffix fallback for very large inputs. The rendered output is
/// capped by both line count and total UTF-8 bytes, so a single multi-megabyte
/// line cannot be copied into the approval card.
fn unified_diff(
    path: &str,
    old: &str,
    new: &str,
    existed: bool,
) -> (String, usize, usize, Option<&'static str>) {
    let ops = diff_ops(old, new);
    let newline_change = trailing_newline_change(old, new);
    let mut additions = 0;
    let mut removals = 0;
    for op in &ops {
        match op {
            DiffOp::Add(_) => additions += 1,
            DiffOp::Remove(_) => removals += 1,
            DiffOp::Same(_) | DiffOp::NoNewline => {}
        }
    }
    let header_old = if existed {
        format!("a/{path}")
    } else {
        "/dev/null".to_string()
    };
    let header_new = format!("b/{path}");
    if additions == 0 && removals == 0 && newline_change.is_none() {
        return (
            format!("--- {header_old}\n+++ {header_new}\n(no content changes)\n"),
            0,
            0,
            None,
        );
    }

    let mut old_line = 1usize;
    let mut new_line = 1usize;
    let mut positions = Vec::with_capacity(ops.len());
    for op in &ops {
        positions.push((old_line, new_line));
        match op {
            DiffOp::Same(_) => {
                old_line += 1;
                new_line += 1;
            }
            DiffOp::Remove(_) => old_line += 1,
            DiffOp::Add(_) => new_line += 1,
            DiffOp::NoNewline => {}
        }
    }

    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(index, op)| (!matches!(op, DiffOp::Same(_))).then_some(index))
        .collect();
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for index in changed {
        let start = index.saturating_sub(DIFF_CONTEXT_LINES);
        let end = (index + DIFF_CONTEXT_LINES + 1).min(ops.len());
        if let Some(last) = groups.last_mut() {
            if start <= last.1 {
                last.1 = end;
                continue;
            }
        }
        groups.push((start, end));
    }

    // Keep room for the truncation marker so the final string never exceeds the
    // advertised cap. All cuts are made at UTF-8 character boundaries.
    let budget = MAX_PREVIEW_BYTES.saturating_sub(DIFF_TRUNCATION_MARKER.len());
    let mut diff = String::new();
    let mut emitted = 0usize;
    let mut truncated = !push_bounded_raw(
        &mut diff,
        &format!("--- {header_old}\n+++ {header_new}\n"),
        budget,
    );
    if !truncated {
        'groups: for (start, end) in groups {
            let (old_start, new_start) = positions[start];
            let old_count = ops[start..end]
                .iter()
                .filter(|op| !matches!(op, DiffOp::Add(_) | DiffOp::NoNewline))
                .count();
            let new_count = ops[start..end]
                .iter()
                .filter(|op| !matches!(op, DiffOp::Remove(_) | DiffOp::NoNewline))
                .count();
            let hunk = format!(
                "@@ -{},{} +{},{} @@\n",
                if old_count == 0 {
                    old_start.saturating_sub(1)
                } else {
                    old_start
                },
                old_count,
                if new_count == 0 {
                    new_start.saturating_sub(1)
                } else {
                    new_start
                },
                new_count
            );
            if !push_bounded_raw(&mut diff, &hunk, budget) {
                truncated = true;
                break;
            }
            for op in &ops[start..end] {
                if emitted >= MAX_PREVIEW_LINES {
                    truncated = true;
                    break 'groups;
                }
                let (prefix, text) = match op {
                    DiffOp::Same(text) => (' ', *text),
                    DiffOp::Remove(text) => ('-', *text),
                    DiffOp::Add(text) => ('+', *text),
                    DiffOp::NoNewline => ('\\', " No newline at end of file"),
                };
                if !push_diff_line(&mut diff, prefix, text, budget) {
                    truncated = true;
                    break 'groups;
                }
                emitted += 1;
            }
        }
    }
    if truncated {
        diff.push_str(DIFF_TRUNCATION_MARKER);
    }
    (diff, additions, removals, newline_change)
}

/// Append at most `budget` bytes, cutting on a character boundary. Returns
/// false when any part of `text` had to be omitted.
fn push_bounded_raw(target: &mut String, text: &str, budget: usize) -> bool {
    if target.len().saturating_add(text.len()) <= budget {
        target.push_str(text);
        return true;
    }
    let available = budget.saturating_sub(target.len());
    let mut end = available.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    target.push_str(&text[..end]);
    false
}

/// Append one diff line, truncating the text (not the prefix) at a character
/// boundary when the remaining byte budget is too small.
fn push_diff_line(target: &mut String, prefix: char, text: &str, budget: usize) -> bool {
    let needed = prefix
        .len_utf8()
        .saturating_add(text.len())
        .saturating_add(1);
    if target.len().saturating_add(needed) <= budget {
        target.push(prefix);
        target.push_str(text);
        target.push('\n');
        return true;
    }
    let available = budget.saturating_sub(target.len());
    let reserved = prefix.len_utf8().saturating_add(1);
    if available > reserved {
        let mut end = (available - reserved).min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end > 0 {
            target.push(prefix);
            target.push_str(&text[..end]);
            target.push('\n');
        }
    }
    false
}

fn diff_ops<'a>(old: &'a str, new: &'a str) -> Vec<DiffOp<'a>> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let mut ops = if a.is_empty() {
        b.iter().map(|line| DiffOp::Add(line)).collect()
    } else if b.is_empty() {
        a.iter().map(|line| DiffOp::Remove(line)).collect()
    } else if a.len().saturating_mul(b.len()) <= LCS_MAX_CELLS
        && a.len().max(b.len()) <= LCS_MAX_ROWS
    {
        lcs_ops(&a, &b)
    } else {
        prefix_suffix_ops(&a, &b)
    };

    // `.lines()` discards the final newline. Mark the last line of whichever
    // side lacks one so a trailing-newline-only mutation is never reported as
    // "no content changes".
    match trailing_newline_change(old, new) {
        Some("removed") => {
            if let Some(index) = ops
                .iter()
                .rposition(|op| matches!(op, DiffOp::Same(_) | DiffOp::Add(_)))
            {
                ops.insert(index + 1, DiffOp::NoNewline);
            }
        }
        Some("added") => {
            if let Some(index) = ops
                .iter()
                .rposition(|op| matches!(op, DiffOp::Same(_) | DiffOp::Remove(_)))
            {
                ops.insert(index + 1, DiffOp::NoNewline);
            }
        }
        _ => {}
    }
    ops
}

fn trailing_newline_change(old: &str, new: &str) -> Option<&'static str> {
    match (old.ends_with('\n'), new.ends_with('\n')) {
        (true, false) if !new.is_empty() => Some("removed"),
        (false, true) if !old.is_empty() => Some("added"),
        _ => None,
    }
}

fn lcs_ops<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<DiffOp<'a>> {
    let mut table = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut ops = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            ops.push(DiffOp::Same(a[i]));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            ops.push(DiffOp::Remove(a[i]));
            i += 1;
        } else {
            ops.push(DiffOp::Add(b[j]));
            j += 1;
        }
    }
    while i < a.len() {
        ops.push(DiffOp::Remove(a[i]));
        i += 1;
    }
    while j < b.len() {
        ops.push(DiffOp::Add(b[j]));
        j += 1;
    }
    ops
}

fn prefix_suffix_ops<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<DiffOp<'a>> {
    let mut prefix = 0usize;
    while prefix < a.len() && prefix < b.len() && a[prefix] == b[prefix] {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < a.len() - prefix
        && suffix < b.len() - prefix
        && a[a.len() - 1 - suffix] == b[b.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let mut ops = Vec::new();
    ops.extend(a[..prefix].iter().map(|line| DiffOp::Same(line)));
    ops.extend(
        a[prefix..a.len() - suffix]
            .iter()
            .map(|line| DiffOp::Remove(line)),
    );
    ops.extend(
        b[prefix..b.len() - suffix]
            .iter()
            .map(|line| DiffOp::Add(line)),
    );
    ops.extend(a[a.len() - suffix..].iter().map(|line| DiffOp::Same(line)));
    ops
}

fn fingerprint(bytes: &[u8]) -> Fingerprint {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Fingerprint {
        len: bytes.len() as u64,
        hash,
    }
}

struct Replacement {
    old: String,
    new: String,
}

/// Accept the advertised `old_string`/`new_string` shape plus Pi-style
/// `edits: [{oldText,newText}]` and the `oldText`/`newText` aliases.
fn replacements(arguments: &Value) -> Result<(Vec<Replacement>, bool, bool), String> {
    let replace_all = arguments
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if let Some(value) = arguments.get("edits") {
        let edits = value
            .as_array()
            .ok_or_else(|| "`edits` must be an array".to_string())?;
        if edits.len() > 100 {
            return Err("at most 100 edits are allowed".to_string());
        }
        if ["old_string", "new_string", "oldText", "newText"]
            .iter()
            .any(|key| arguments.get(*key).is_some())
        {
            return Err("use either edits or a single replacement, not both".to_string());
        }
        if edits.is_empty() {
            return Err("`edits` must contain at least one replacement".to_string());
        }
        if replace_all {
            return Err("`replace_all` cannot be combined with `edits`".to_string());
        }
        let mut parts = Vec::with_capacity(edits.len());
        for (index, edit) in edits.iter().enumerate() {
            let old = edit
                .get("oldText")
                .or_else(|| edit.get("old_string"))
                .and_then(Value::as_str)
                .ok_or_else(|| format!("edits[{index}] is missing `oldText`/`old_string`"))?;
            let new = edit
                .get("newText")
                .or_else(|| edit.get("new_string"))
                .and_then(Value::as_str)
                .ok_or_else(|| format!("edits[{index}] is missing `newText`/`new_string`"))?;
            parts.push(Replacement {
                old: old.to_string(),
                new: new.to_string(),
            });
        }
        return Ok((parts, true, false));
    }

    let old = arguments
        .get("old_string")
        .or_else(|| arguments.get("oldText"))
        .and_then(Value::as_str)
        .ok_or_else(|| "missing required string argument `old_string`".to_string())?;
    let new = arguments
        .get("new_string")
        .or_else(|| arguments.get("newText"))
        .and_then(Value::as_str)
        .ok_or_else(|| "missing required string argument `new_string`".to_string())?;
    Ok((
        vec![Replacement {
            old: old.to_string(),
            new: new.to_string(),
        }],
        false,
        replace_all,
    ))
}

fn apply_replacements(
    original: &str,
    replacements: &[Replacement],
    simultaneous: bool,
    replace_all: bool,
) -> Result<(String, usize), String> {
    for replacement in replacements {
        if replacement.old.is_empty() {
            return Err("the old text must not be empty".to_string());
        }
        if replacement.old == replacement.new {
            return Err("the old and new text are identical".to_string());
        }
    }

    if !simultaneous && replacements.len() == 1 && replace_all {
        let replacement = &replacements[0];
        let count = original.matches(&replacement.old).count();
        if count == 0 {
            return Err(format!(
                "old text not found; no changes made. Searched for: {}",
                snippet(&replacement.old)
            ));
        }
        return Ok((original.replace(&replacement.old, &replacement.new), count));
    }

    let mut ranges: Vec<(usize, usize, &str)> = Vec::new();
    for replacement in replacements {
        let matches: Vec<usize> = original
            .match_indices(&replacement.old)
            .map(|(index, _)| index)
            .collect();
        match matches.len() {
            0 => {
                return Err(format!(
                    "old text not found; no changes made. Searched for: {}",
                    snippet(&replacement.old)
                ))
            }
            1 => ranges.push((
                matches[0],
                matches[0] + replacement.old.len(),
                replacement.new.as_str(),
            )),
            count => {
                return Err(format!(
                    "old text occurs {count} times; include more surrounding context or set replace_all: true. Searched for: {}",
                    snippet(&replacement.old)
                ))
            }
        }
    }

    ranges.sort_by_key(|(start, _, _)| *start);
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err("edits overlap; merge them into one replacement".to_string());
        }
    }

    let mut updated = original.to_string();
    for (start, end, new) in ranges.iter().rev() {
        updated.replace_range(*start..*end, new);
    }
    Ok((updated, ranges.len()))
}

fn snippet(text: &str) -> String {
    let mut preview: String = text.chars().take(120).collect();
    if text.chars().count() > 120 {
        preview.push('…');
    }
    format!("{preview:?}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn context() -> (TempDir, ToolContext) {
        let temp = TempDir::new();
        let root = paths::canonical_root(&temp.path().join("project")).unwrap();
        let ctx = ToolContext {
            root,
            allow_shell: true,
            skills: Vec::new(),
            writes: Default::default(),
        };
        (temp, ctx)
    }

    #[test]
    fn write_then_read_round_trip_and_creates_parents() {
        let (_temp, ctx) = context();
        let written = write(
            &ctx,
            &serde_json::json!({"path":"nested/dir/file.txt","content":"one\ntwo\nthree"}),
        );
        assert!(!written.is_error, "{}", written.content);
        assert!(ctx.root.join("nested/dir/file.txt").is_file());

        let read_back = read(&ctx, &serde_json::json!({"path":"nested/dir/file.txt"}));
        assert!(!read_back.is_error);
        assert_eq!(read_back.content, "one\ntwo\nthree");
    }

    #[test]
    fn read_honors_offset_and_limit_and_reports_more() {
        let (_temp, ctx) = context();
        let _ = write(
            &ctx,
            &serde_json::json!({"path":"f.txt","content":"1\n2\n3\n4\n5"}),
        );
        let outcome = read(
            &ctx,
            &serde_json::json!({"path":"f.txt","offset":2,"limit":2}),
        );
        assert!(!outcome.is_error);
        assert!(outcome.content.starts_with("2\n3"), "{}", outcome.content);
        assert!(outcome.content.contains("Use offset=4"));
    }

    #[test]
    fn read_reports_empty_directory_binary_and_bad_offset() {
        let (_temp, ctx) = context();
        write(&ctx, &serde_json::json!({"path":"empty","content":""}));
        assert_eq!(
            read(&ctx, &serde_json::json!({"path":"empty"})).content,
            "(empty file)"
        );

        std::fs::create_dir_all(ctx.root.join("dir")).unwrap();
        assert!(read(&ctx, &serde_json::json!({"path":"dir"})).is_error);

        std::fs::write(ctx.root.join("bin"), b"a\0b").unwrap();
        assert!(read(&ctx, &serde_json::json!({"path":"bin"})).is_error);

        write(&ctx, &serde_json::json!({"path":"one","content":"x"}));
        assert!(read(&ctx, &serde_json::json!({"path":"one","offset":5})).is_error);
    }

    #[test]
    fn read_rejects_invalid_utf8_and_binary_data_beyond_header() {
        let (_temp, ctx) = context();
        std::fs::write(ctx.root.join("invalid"), [0xff, 0xfe]).unwrap();
        assert!(read(&ctx, &serde_json::json!({"path":"invalid"})).is_error);
        let mut bytes = vec![b'x'; 9000];
        bytes.push(0);
        std::fs::write(ctx.root.join("binary"), bytes).unwrap();
        assert!(read(&ctx, &serde_json::json!({"path":"binary"})).is_error);
    }

    #[test]
    fn append_preserves_content_and_confines_paths() {
        let (_temp, ctx) = context();
        assert!(
            !append(
                &ctx,
                &serde_json::json!({"path":"notes/day.txt","content":"first\n"})
            )
            .is_error
        );
        assert!(
            !append(
                &ctx,
                &serde_json::json!({"path":"notes/day.txt","content":"second\n"})
            )
            .is_error
        );
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("notes/day.txt")).unwrap(),
            "first\nsecond\n"
        );
        assert!(
            append(
                &ctx,
                &serde_json::json!({"path":"../outside","content":"x"})
            )
            .is_error
        );
        assert!(append(&ctx, &serde_json::json!({"path":"notes","content":"x"})).is_error);
    }

    #[test]
    fn file_tools_reject_escapes() {
        let (_temp, ctx) = context();
        let escape = read(&ctx, &serde_json::json!({"path":"../secret"}));
        assert!(escape.is_error);
        assert!(escape.content.contains("escapes project root"));

        let write_escape = write(
            &ctx,
            &serde_json::json!({"path":"../secret","content":"nope"}),
        );
        assert!(write_escape.is_error);
    }

    #[cfg(unix)]
    #[test]
    fn write_through_symlinked_directory_is_rejected() {
        let (temp, ctx) = context();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, ctx.root.join("link")).unwrap();

        let outcome = write(
            &ctx,
            &serde_json::json!({"path":"link/new.txt","content":"nope"}),
        );
        assert!(outcome.is_error);
        assert!(!outside.join("new.txt").exists());
    }

    #[test]
    fn edit_requires_unique_match_unless_replace_all() {
        let (_temp, ctx) = context();
        write(
            &ctx,
            &serde_json::json!({"path":"f.txt","content":"alpha beta alpha"}),
        );
        let ambiguous = edit(
            &ctx,
            &serde_json::json!({"path":"f.txt","old_string":"alpha","new_string":"x"}),
        );
        assert!(ambiguous.is_error);
        assert!(ambiguous.content.contains("occurs 2 times"));
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("f.txt")).unwrap(),
            "alpha beta alpha"
        );

        let all = edit(
            &ctx,
            &serde_json::json!({
                "path":"f.txt","old_string":"alpha","new_string":"x","replace_all":true
            }),
        );
        assert!(!all.is_error, "{}", all.content);
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("f.txt")).unwrap(),
            "x beta x"
        );
    }

    #[test]
    fn edit_reports_missing_text_and_identical_text() {
        let (_temp, ctx) = context();
        write(&ctx, &serde_json::json!({"path":"f.txt","content":"hello"}));
        let missing = edit(
            &ctx,
            &serde_json::json!({"path":"f.txt","old_string":"nope","new_string":"x"}),
        );
        assert!(missing.is_error);
        assert!(missing.content.contains("not found"));

        let identical = edit(
            &ctx,
            &serde_json::json!({"path":"f.txt","old_string":"hello","new_string":"hello"}),
        );
        assert!(identical.is_error);
    }

    #[test]
    fn read_long_unicode_line_keeps_a_valid_boundary() {
        let (_temp, ctx) = context();
        std::fs::write(ctx.root.join("long.txt"), "界".repeat(20000)).unwrap();
        let result = read(&ctx, &serde_json::json!({"path":"long.txt"}));
        assert!(!result.is_error);
        assert!(result.content.contains("truncated"));
    }

    #[test]
    fn batch_edit_is_all_or_nothing_and_rejects_stale_content() {
        let (_temp, ctx) = context();
        let path = ctx.root.join("draft.txt");
        std::fs::write(&path, "one two three").unwrap();
        let failed = edit(
            &ctx,
            &serde_json::json!({"path":"draft.txt", "edits":[
                {"old_string":"one", "new_string":"1"},
                {"old_string":"missing", "new_string":"2"}
            ]}),
        );
        assert!(failed.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one two three");
        let stale = edit(
            &ctx,
            &serde_json::json!({"path":"draft.txt", "expected_content":"old version", "old_string":"one", "new_string":"1"}),
        );
        assert!(stale.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one two three");
        let ok = edit(
            &ctx,
            &serde_json::json!({"path":"draft.txt", "expected_content":"one two three", "edits":[
                {"old_string":"one", "new_string":"1"},
                {"old_string":"three", "new_string":"3"}
            ]}),
        );
        assert!(!ok.is_error, "{}", ok.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "1 two 3");
        assert!(
            edit(
                &ctx,
                &serde_json::json!({"path":"draft.txt", "edits":"invalid"})
            )
            .is_error
        );
        assert!(edit(&ctx, &serde_json::json!({"path":"draft.txt", "edits":[{"old_string":"1 two", "new_string":"a"},{"old_string":"two 3", "new_string":"b"}]})).is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "1 two 3");
    }

    #[test]
    fn edit_supports_pi_style_multiple_edits() {
        let (_temp, ctx) = context();
        write(
            &ctx,
            &serde_json::json!({"path":"f.txt","content":"one two three"}),
        );
        let outcome = edit(
            &ctx,
            &serde_json::json!({
                "path":"f.txt",
                "edits":[
                    {"oldText":"one","newText":"1"},
                    {"oldText":"three","newText":"3"}
                ]
            }),
        );
        assert!(!outcome.is_error, "{}", outcome.content);
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("f.txt")).unwrap(),
            "1 two 3"
        );
    }

    #[test]
    fn overwrite_and_append_back_up_original_bytes() {
        let (_temp, ctx) = context();
        std::fs::write(ctx.root.join("note.txt"), "original\n").unwrap();
        let read_back = read(&ctx, &serde_json::json!({"path":"note.txt"}));
        assert!(!read_back.is_error);

        let overwritten = write(
            &ctx,
            &serde_json::json!({"path":"note.txt","content":"replacement\n"}),
        );
        assert!(!overwritten.is_error, "{}", overwritten.content);
        assert!(overwritten.content.contains(".drops/backups/"));
        let backups = std::fs::read_dir(ctx.root.join(".drops/backups"))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), "original\n");

        let appended = append(
            &ctx,
            &serde_json::json!({"path":"note.txt","content":"more\n"}),
        );
        assert!(!appended.is_error, "{}", appended.content);
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("note.txt")).unwrap(),
            "replacement\nmore\n"
        );
        assert_eq!(
            std::fs::read_dir(ctx.root.join(".drops/backups"))
                .unwrap()
                .count(),
            2
        );
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replacement_preserves_mode() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, ctx) = context();
        let path = ctx.root.join("script.sh");
        std::fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o750)).unwrap();
        assert!(!read(&ctx, &serde_json::json!({"path":"script.sh"})).is_error);
        let outcome = write(
            &ctx,
            &serde_json::json!({"path":"script.sh","content":"#!/bin/sh\necho new\n"}),
        );
        assert!(!outcome.is_error, "{}", outcome.content);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o750
        );
    }

    #[test]
    fn read_then_external_change_is_refused_without_mutation() {
        let (_temp, ctx) = context();
        let path = ctx.root.join("draft.txt");
        std::fs::write(&path, "one two").unwrap();
        assert!(!read(&ctx, &serde_json::json!({"path":"draft.txt"})).is_error);
        std::fs::write(&path, "changed by someone else").unwrap();

        let outcome = edit(
            &ctx,
            &serde_json::json!({"path":"draft.txt","old_string":"one","new_string":"1"}),
        );
        assert!(outcome.is_error);
        assert!(
            outcome.content.contains("changed after it was read"),
            "{}",
            outcome.content
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "changed by someone else"
        );
        assert!(!ctx.root.join(".drops/backups").exists());
    }

    #[test]
    fn previews_are_concrete_and_read_only() {
        let (_temp, ctx) = context();
        std::fs::write(ctx.root.join("note.txt"), "one\ntwo\nthree\n").unwrap();
        assert!(!read(&ctx, &serde_json::json!({"path":"note.txt"})).is_error);

        let preview = preview_edit(
            &ctx,
            &serde_json::json!({"path":"note.txt","old_string":"two","new_string":"TWO"}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(preview["kind"], "edit");
        assert_eq!(preview["path"], "note.txt");
        let diff = preview["diff"].as_str().unwrap();
        assert!(diff.contains("-two"), "{diff}");
        assert!(diff.contains("+TWO"), "{diff}");
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("note.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
        assert!(!ctx.root.join(".drops/backups").exists());

        let err = preview_edit(
            &ctx,
            &serde_json::json!({"path":"note.txt","old_string":"missing","new_string":"x"}),
        )
        .unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn preview_write_shows_added_lines_without_creating_the_file() {
        let (_temp, ctx) = context();
        let preview = preview_write(
            &ctx,
            &serde_json::json!({"path":"new.txt","content":"hello\nworld\n"}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(preview["existing"], false);
        assert!(preview["diff"].as_str().unwrap().contains("+hello"));
        assert!(preview["diff"].as_str().unwrap().contains("+world"));
        assert!(!ctx.root.join("new.txt").exists());
    }

    #[test]
    fn backup_history_is_bounded_per_file() {
        let (_temp, ctx) = context();
        for index in 0..(MAX_BACKUPS_PER_FILE + 3) {
            let outcome = write(
                &ctx,
                &serde_json::json!({"path":"log.txt","content":format!("version {index}\n")}),
            );
            assert!(!outcome.is_error, "{}", outcome.content);
        }
        let backups = std::fs::read_dir(ctx.root.join(".drops/backups"))
            .unwrap()
            .flatten()
            .count();
        assert_eq!(backups, MAX_BACKUPS_PER_FILE);
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("log.txt")).unwrap(),
            format!("version {}\n", MAX_BACKUPS_PER_FILE + 2)
        );
    }

    #[test]
    fn binary_overwrite_is_refused_before_mutation() {
        let (_temp, ctx) = context();
        std::fs::write(ctx.root.join("image.bin"), b"\x89PNG\0data").unwrap();
        let outcome = write(
            &ctx,
            &serde_json::json!({"path":"image.bin","content":"text"}),
        );
        assert!(outcome.is_error);
        assert!(outcome.content.contains("binary"), "{}", outcome.content);
        assert_eq!(
            std::fs::read(ctx.root.join("image.bin")).unwrap(),
            b"\x89PNG\0data"
        );
    }

    #[test]
    fn preview_freezes_create_target_until_execution() {
        let (_temp, ctx) = context();
        let arguments = serde_json::json!({"path":"new.txt","content":"approved\n"});
        let preview = preview_write(&ctx, &arguments).unwrap().unwrap();
        assert_eq!(preview["existing"], false);
        assert!(!ctx.root.join("new.txt").exists());

        // No read() happened, so only the prepared snapshot can catch this.
        std::fs::write(ctx.root.join("new.txt"), "appeared\n").unwrap();
        let outcome = write(&ctx, &arguments);
        assert!(outcome.is_error, "{}", outcome.content);
        assert!(
            outcome
                .content
                .contains("appeared after the approved create preview"),
            "{}",
            outcome.content
        );
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("new.txt")).unwrap(),
            "appeared\n"
        );
        assert!(!ctx.root.join(".drops/backups").exists());
    }

    #[test]
    fn preview_freezes_overwrite_and_refuses_deletion_or_external_change() {
        let (_temp, ctx) = context();
        let path = ctx.root.join("note.txt");
        std::fs::write(&path, "original\n").unwrap();
        let arguments = serde_json::json!({"path":"note.txt","content":"replacement\n"});
        let _ = preview_write(&ctx, &arguments).unwrap().unwrap();

        std::fs::remove_file(&path).unwrap();
        let deleted = write(&ctx, &arguments);
        assert!(deleted.is_error, "{}", deleted.content);
        assert!(
            deleted
                .content
                .contains("deleted after the approved preview"),
            "{}",
            deleted.content
        );
        assert!(!path.exists());

        // A fresh preview is required after the refused execution.
        std::fs::write(&path, "original\n").unwrap();
        let _ = preview_write(&ctx, &arguments).unwrap().unwrap();
        std::fs::write(&path, "tampered\n").unwrap();
        let changed = write(&ctx, &arguments);
        assert!(changed.is_error, "{}", changed.content);
        assert!(
            changed
                .content
                .contains("changed after it was read for the approved preview"),
            "{}",
            changed.content
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "tampered\n");
    }

    #[test]
    fn preview_then_execution_without_changes_succeeds() {
        let (_temp, ctx) = context();
        let path = ctx.root.join("note.txt");
        std::fs::write(&path, "one\n").unwrap();
        let arguments = serde_json::json!({"path":"note.txt","content":"two\n"});
        let _ = preview_write(&ctx, &arguments).unwrap().unwrap();
        let outcome = write(&ctx, &arguments);
        assert!(!outcome.is_error, "{}", outcome.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two\n");
    }

    #[test]
    fn preview_append_refuses_data_changed_after_preview() {
        let (_temp, ctx) = context();
        let path = ctx.root.join("log.txt");
        std::fs::write(&path, "one\n").unwrap();
        let arguments = serde_json::json!({"path":"log.txt","content":"two\n"});
        let _ = preview_append(&ctx, &arguments).unwrap().unwrap();
        std::fs::write(&path, "one\nthree\n").unwrap();
        let outcome = append(&ctx, &arguments);
        assert!(outcome.is_error, "{}", outcome.content);
        assert!(
            outcome
                .content
                .contains("changed after it was read for the approved preview"),
            "{}",
            outcome.content
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\nthree\n");
    }

    #[test]
    fn preview_edit_refuses_data_changed_after_preview() {
        let (_temp, ctx) = context();
        let path = ctx.root.join("edit.txt");
        std::fs::write(&path, "one two").unwrap();
        let arguments =
            serde_json::json!({"path":"edit.txt","old_string":"two","new_string":"TWO"});
        let _ = preview_edit(&ctx, &arguments).unwrap().unwrap();
        std::fs::write(&path, "one three").unwrap();
        let outcome = edit(&ctx, &arguments);
        assert!(outcome.is_error, "{}", outcome.content);
        assert!(
            outcome
                .content
                .contains("changed after it was read for the approved preview"),
            "{}",
            outcome.content
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one three");
    }

    #[cfg(unix)]
    #[test]
    fn preview_refuses_symlink_retarget_inside_workspace() {
        let (_temp, ctx) = context();
        std::fs::write(ctx.root.join("a.txt"), "A\n").unwrap();
        std::fs::write(ctx.root.join("b.txt"), "B\n").unwrap();
        std::os::unix::fs::symlink("a.txt", ctx.root.join("link.txt")).unwrap();

        let arguments = serde_json::json!({"path":"link.txt","content":"new\n"});
        let preview = preview_write(&ctx, &arguments).unwrap().unwrap();
        assert_eq!(preview["existing"], true);

        std::fs::remove_file(ctx.root.join("link.txt")).unwrap();
        std::os::unix::fs::symlink("b.txt", ctx.root.join("link.txt")).unwrap();
        let outcome = write(&ctx, &arguments);
        assert!(outcome.is_error, "{}", outcome.content);
        assert!(
            outcome.content.contains("different file"),
            "{}",
            outcome.content
        );
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("a.txt")).unwrap(),
            "A\n"
        );
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("b.txt")).unwrap(),
            "B\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn backup_directory_symlink_outside_root_is_rejected_before_creation() {
        let (temp, ctx) = context();
        let outside = temp.path().join("outside-backups");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, ctx.root.join(".drops")).unwrap();
        std::fs::write(ctx.root.join("note.txt"), "original\n").unwrap();

        let outcome = write(
            &ctx,
            &serde_json::json!({"path":"note.txt","content":"replacement\n"}),
        );
        assert!(outcome.is_error, "{}", outcome.content);
        assert!(
            outcome.content.contains("escapes project root"),
            "{}",
            outcome.content
        );
        assert!(!outside.join("backups").exists());
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("note.txt")).unwrap(),
            "original\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn backups_are_private_and_preserve_source_mode() {
        use std::os::unix::fs::PermissionsExt;
        let (_temp, ctx) = context();
        let path = ctx.root.join("secret.txt");
        std::fs::write(&path, "private\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let outcome = write(
            &ctx,
            &serde_json::json!({"path":"secret.txt","content":"changed\n"}),
        );
        assert!(!outcome.is_error, "{}", outcome.content);
        let backup = std::fs::read_dir(ctx.root.join(".drops/backups"))
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .path();
        assert_eq!(
            std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "private\n");
    }

    #[test]
    fn backup_pruning_is_isolated_per_target_identity() {
        let (_temp, ctx) = context();
        std::fs::create_dir_all(ctx.root.join("a")).unwrap();
        std::fs::write(ctx.root.join("a/b.txt"), "slash-original\n").unwrap();
        std::fs::write(ctx.root.join("a_b.txt"), "underscore-original\n").unwrap();
        std::fs::write(ctx.root.join("pre.b"), "prefix-original\n").unwrap();
        std::fs::write(ctx.root.join("pre"), "pre-original\n").unwrap();

        for (path, content) in [
            ("a_b.txt", "underscore-updated\n"),
            ("pre.b", "prefix-updated\n"),
        ] {
            let outcome = write(&ctx, &serde_json::json!({"path":path, "content":content}));
            assert!(!outcome.is_error, "{}", outcome.content);
        }
        // Make the colliding/prefix targets' backups unambiguously older.
        std::thread::sleep(std::time::Duration::from_millis(20));

        for index in 0..(MAX_BACKUPS_PER_FILE + 3) {
            let outcome = write(
                &ctx,
                &serde_json::json!({"path":"a/b.txt", "content": format!("slash-{index}\n")}),
            );
            assert!(!outcome.is_error, "{}", outcome.content);
            let outcome = write(
                &ctx,
                &serde_json::json!({"path":"pre", "content": format!("pre-{index}\n")}),
            );
            assert!(!outcome.is_error, "{}", outcome.content);
        }

        let contents: Vec<String> = std::fs::read_dir(ctx.root.join(".drops/backups"))
            .unwrap()
            .flatten()
            .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
            .collect();
        assert!(
            contents
                .iter()
                .any(|content| content == "underscore-original\n"),
            "{contents:?}"
        );
        assert!(
            contents
                .iter()
                .any(|content| content == "prefix-original\n"),
            "{contents:?}"
        );
        // The two churned targets each kept their ten newest backups.
        assert!(
            contents.iter().any(|content| content == "slash-11\n"),
            "{contents:?}"
        );
        assert!(
            contents.iter().any(|content| content == "pre-11\n"),
            "{contents:?}"
        );
        assert_eq!(
            contents.len(),
            (MAX_BACKUPS_PER_FILE * 2) + 2,
            "{contents:?}"
        );
    }

    #[test]
    fn preview_diff_caps_total_bytes_and_warns_before_approval() {
        let (_temp, ctx) = context();
        let old: String = (0..200)
            .map(|index| format!("old line {index} {}\n", "a".repeat(300)))
            .collect();
        let new: String = (0..200)
            .map(|index| format!("new line {index} {}\n", "b".repeat(300)))
            .collect();
        std::fs::write(ctx.root.join("big.txt"), &old).unwrap();

        let preview = preview_write(&ctx, &serde_json::json!({"path":"big.txt","content":new}))
            .unwrap()
            .unwrap();
        let diff = preview["diff"].as_str().unwrap();
        assert!(diff.len() <= MAX_PREVIEW_BYTES, "{} bytes", diff.len());
        assert!(diff.contains("truncated"), "{diff}");
        assert!(diff.contains("before approving"), "{diff}");
        assert_eq!(
            std::fs::read_to_string(ctx.root.join("big.txt")).unwrap(),
            old
        );
    }

    #[test]
    fn preview_diff_truncates_a_single_huge_line_without_panicking() {
        let (_temp, ctx) = context();
        let old = format!("{}\n", "界".repeat(30_000));
        let new = format!("{}\n", "語".repeat(30_000));
        std::fs::write(ctx.root.join("huge.txt"), &old).unwrap();

        let preview = preview_write(&ctx, &serde_json::json!({"path":"huge.txt","content":new}))
            .unwrap()
            .unwrap();
        let diff = preview["diff"].as_str().unwrap();
        assert!(diff.len() <= MAX_PREVIEW_BYTES, "{} bytes", diff.len());
        assert!(diff.contains("truncated"), "{diff}");
    }

    #[test]
    fn preview_diff_reports_final_newline_changes() {
        let (_temp, ctx) = context();
        std::fs::write(ctx.root.join("nl.txt"), "hello\n").unwrap();
        let removed = preview_write(
            &ctx,
            &serde_json::json!({"path":"nl.txt","content":"hello"}),
        )
        .unwrap()
        .unwrap();
        assert_ne!(removed["summary"], "No content changes.");
        assert!(
            removed["summary"]
                .as_str()
                .unwrap()
                .contains("Final newline removed"),
            "{}",
            removed["summary"]
        );
        assert!(
            removed["diff"]
                .as_str()
                .unwrap()
                .contains("No newline at end of file"),
            "{}",
            removed["diff"]
        );

        std::fs::write(ctx.root.join("nl2.txt"), "hello").unwrap();
        let added = preview_write(
            &ctx,
            &serde_json::json!({"path":"nl2.txt","content":"hello\n"}),
        )
        .unwrap()
        .unwrap();
        assert!(
            added["summary"]
                .as_str()
                .unwrap()
                .contains("Final newline added"),
            "{}",
            added["summary"]
        );
        assert!(
            added["diff"]
                .as_str()
                .unwrap()
                .contains("No newline at end of file"),
            "{}",
            added["diff"]
        );

        let unchanged = preview_write(
            &ctx,
            &serde_json::json!({"path":"nl.txt","content":"hello\n"}),
        )
        .unwrap()
        .unwrap();
        assert_eq!(unchanged["summary"], "No content changes.");
    }

    #[test]
    fn diff_handles_empty_inputs_and_many_tiny_lines() {
        let (added, additions, removals, _) = unified_diff("x", "", "a\nb\n", false);
        assert_eq!((additions, removals), (2, 0));
        assert!(added.contains("+a"), "{added}");

        let (removed, additions, removals, _) = unified_diff("x", "a\nb\n", "", true);
        assert_eq!((additions, removals), (0, 2));
        assert!(removed.contains("-a"), "{removed}");

        let old = "x\n".repeat(30_000);
        let new = "y\n".repeat(30_000);
        let (diff, additions, removals, _) = unified_diff("x", &old, &new, true);
        assert_eq!((additions, removals), (30_000, 30_000));
        assert!(diff.len() <= MAX_PREVIEW_BYTES, "{} bytes", diff.len());
        assert!(diff.contains("truncated"), "{diff}");
    }

    #[test]
    fn preview_write_and_write_enforce_the_result_size_limit() {
        let (_temp, ctx) = context();
        let too_big = "x".repeat(MAX_WRITE_FILE_BYTES as usize + 1);
        let preview = preview_write(
            &ctx,
            &serde_json::json!({"path":"big.txt","content":too_big}),
        );
        assert!(preview.is_err());
        assert!(preview.unwrap_err().contains("limit"));

        let too_big = "x".repeat(MAX_WRITE_FILE_BYTES as usize + 1);
        let outcome = write(
            &ctx,
            &serde_json::json!({"path":"big.txt","content":too_big}),
        );
        assert!(outcome.is_error, "{}", outcome.content);
        assert!(outcome.content.contains("limit"), "{}", outcome.content);
        assert!(!ctx.root.join("big.txt").exists());
    }

    #[test]
    fn preview_append_and_edit_enforce_the_result_size_limit() {
        let (_temp, ctx) = context();
        std::fs::write(ctx.root.join("base.txt"), "seed\n").unwrap();
        let too_big = "x".repeat(MAX_WRITE_FILE_BYTES as usize);
        let appended = preview_append(
            &ctx,
            &serde_json::json!({"path":"base.txt","content":too_big}),
        );
        assert!(appended.is_err());
        assert!(appended.unwrap_err().contains("limit"));

        let too_big = "x".repeat(MAX_WRITE_FILE_BYTES as usize);
        let edited = preview_edit(
            &ctx,
            &serde_json::json!({"path":"base.txt","old_string":"seed","new_string":too_big}),
        );
        assert!(edited.is_err());
        assert!(edited.unwrap_err().contains("limit"));
    }
}
