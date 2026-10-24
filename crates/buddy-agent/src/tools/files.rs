//! File tools. Every path goes through `paths::confine` first.

use std::path::Path;

use serde_json::Value;

use crate::paths;
use crate::tools::{optional_u64, string_arg, ToolContext, ToolOutcome};

const DEFAULT_READ_LINES: usize = 2_000;
const MAX_READ_LINES: usize = 5_000;
const MAX_READ_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_READ_OUTPUT_BYTES: usize = 50_000;

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
    if metadata.is_dir() {
        return ToolOutcome::error(format!(
            "read: `{}` is a directory, not a file.",
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

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            return ToolOutcome::error(format!(
                "read: cannot read `{}`: {err}",
                paths::display_path(&ctx.root, &path)
            ))
        }
    };
    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return ToolOutcome::error(format!(
            "read: `{}` looks like a binary file.",
            paths::display_path(&ctx.root, &path)
        ));
    }

    let text = String::from_utf8_lossy(&bytes);
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

    let path = match paths::confine(&ctx.root, requested) {
        Ok(path) => path,
        Err(err) => return ToolOutcome::error(format!("write: {err:#}")),
    };
    if path.exists() && path.is_dir() {
        return ToolOutcome::error(format!(
            "write: `{}` is a directory.",
            paths::display_path(&ctx.root, &path)
        ));
    }

    let path = match ensure_parent(&ctx.root, &path) {
        Ok(path) => path,
        Err(message) => return ToolOutcome::error(format!("write: {message}")),
    };

    match std::fs::write(&path, content.as_bytes()) {
        Ok(()) => ToolOutcome::ok(format!(
            "Successfully wrote {} bytes to `{}`.",
            content.len(),
            paths::display_path(&ctx.root, &path)
        )),
        Err(err) => ToolOutcome::error(format!(
            "write: failed to write `{}`: {err}",
            paths::display_path(&ctx.root, &path)
        )),
    }
}

pub(crate) fn append(ctx: &ToolContext, arguments: &Value) -> ToolOutcome {
    use std::io::Write;
    let result = (|| -> anyhow::Result<String> {
        let requested = arguments
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string argument `path`"))?;
        let content = arguments
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string argument `content`"))?;
        let path = paths::confine(&ctx.root, requested)?;
        if path.exists() && !path.is_file() {
            anyhow::bail!("target must be a regular file");
        }
        let path = ensure_parent(&ctx.root, &path).map_err(anyhow::Error::msg)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        file.write_all(content.as_bytes())?;
        Ok(format!(
            "Appended {} bytes to `{}`.",
            content.len(),
            paths::display_path(&ctx.root, &path)
        ))
    })();
    match result {
        Ok(message) => ToolOutcome::ok(message),
        Err(err) => ToolOutcome::error(format!("append: {err:#}")),
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
    if path.is_dir() {
        return ToolOutcome::error(format!("edit: `{display}` is a directory."));
    }

    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() && meta.len() <= MAX_READ_FILE_BYTES => {}
        _ => {
            return ToolOutcome::error("edit: target must be a regular file no larger than 8 MiB.")
        }
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    let read_result = std::fs::File::open(&path)
        .and_then(|file| file.take(MAX_READ_FILE_BYTES + 1).read_to_end(&mut bytes));
    if let Err(err) = read_result {
        return ToolOutcome::error(format!("edit: cannot read `{display}`: {err}"));
    }
    if bytes.len() as u64 > MAX_READ_FILE_BYTES {
        return ToolOutcome::error("edit: file exceeds 8 MiB limit");
    }
    let original = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            return ToolOutcome::error(format!(
                "edit: `{display}` is not valid UTF-8 text; use write to replace it."
            ))
        }
    };

    if let Some(expected) = arguments.get("expected_content") {
        if expected.as_str() != Some(original.as_str()) {
            return ToolOutcome::error("edit: file changed since it was read; read it again before editing. No changes made.");
        }
    }

    let (updated, count) =
        match apply_replacements(&original, &replacements, simultaneous, replace_all) {
            Ok(result) => result,
            Err(message) => return ToolOutcome::error(format!("edit: {message}")),
        };

    let path = match ensure_parent(&ctx.root, &path) {
        Ok(path) => path,
        Err(message) => return ToolOutcome::error(format!("edit: {message}")),
    };
    match std::fs::write(&path, updated.as_bytes()) {
        Ok(()) => ToolOutcome::ok(format!(
            "Successfully edited `{display}` ({count} replacement{}).",
            if count == 1 { "" } else { "s" }
        )),
        Err(err) => ToolOutcome::error(format!("edit: failed to write `{display}`: {err}")),
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
}
