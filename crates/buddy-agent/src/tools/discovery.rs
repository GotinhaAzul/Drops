//! Bounded workspace discovery without granting shell access.
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::{optional_u64, string_arg, ToolContext, ToolOutcome};
use crate::paths;

pub(super) fn definition(search: bool) -> Value {
    let mut properties = json!({
        "path": {"type":"string", "description":"Workspace directory, or file for search. Defaults to '.' ."},
        "offset": {"type":"integer", "minimum":0, "description":"Skip this many results; use next_offset to continue. Rescans current files, so results may change."},
        "limit": {"type":"integer", "minimum":1, "maximum":200, "description":"Maximum entries or matching lines (default 100)."}
    });
    if search {
        properties["query"] = json!({"type":"string", "minLength":1, "maxLength":512, "description":"Literal text, not a regular expression."});
        properties["case_sensitive"] = json!({"type":"boolean", "description":"Default true; false uses ASCII case-insensitive matching."});
        properties["file_pattern"] = json!({"type":"string", "description":"Optional glob matched against the workspace-relative path, e.g. **/*.rs."});
        properties["include_hidden"] = json!({"type":"boolean", "description":"Include hidden entries (default false). Symlinks and .git are always skipped."});
        properties["include_ignored"] = json!({"type":"boolean", "description":"Include gitignored files and common generated/dependency directories (default false)."});
    }
    json!({"type":"function", "function": {
        "name": if search {"search_files"} else {"list_files"},
        "description": if search {
            "Search workspace UTF-8 text recursively with line numbers and match-centered excerpts. Respects .gitignore and skips generated directories by default. Never follows symlinks. Returns scan statistics, warnings and pagination; scan_limited means the scan budget prevented complete coverage."
        } else {
            "List one workspace directory, including hidden names, with entry types and sizes. Does not follow symlinks. Supports pagination with next_offset."
        },
        "parameters":{"type":"object", "properties":properties, "required":if search {vec!["query"]} else {vec![]}}
    }})
}

pub(super) fn execute(
    ctx: &ToolContext,
    args: &Value,
    search: bool,
    cancel: &CancellationToken,
) -> ToolOutcome {
    match run(ctx, args, search, cancel) {
        Ok(value) => ToolOutcome::ok(value.to_string()),
        Err(err) => ToolOutcome::error(format!("file discovery: {err:#}")),
    }
}

fn boolean(args: &Value, name: &str, default: bool) -> anyhow::Result<bool> {
    match args.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("{name} must be a boolean")),
    }
}

fn run(
    ctx: &ToolContext,
    args: &Value,
    search: bool,
    cancel: &CancellationToken,
) -> anyhow::Result<Value> {
    let requested = match args.get("path") {
        None => ".",
        Some(value) => value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("path must be a string"))?,
    };
    let start = paths::confine(&ctx.root, requested)?;
    let limit = optional_u64(args, "limit").unwrap_or(100).clamp(1, 200) as usize;
    let offset = usize::try_from(optional_u64(args, "offset").unwrap_or(0)).unwrap_or(usize::MAX);
    let query = if search {
        string_arg(args, "query").map_err(|e| anyhow::anyhow!(e.content))?
    } else {
        ""
    };
    if search && (query.is_empty() || query.chars().count() > 512) {
        anyhow::bail!("query must contain 1–512 characters");
    }
    if !start.exists() {
        anyhow::bail!("path does not exist");
    }
    if !search && !start.is_dir() {
        anyhow::bail!("list_files requires a directory");
    }
    let case_sensitive = boolean(args, "case_sensitive", true)?;
    let include_hidden = boolean(args, "include_hidden", false)?;
    let include_ignored = boolean(args, "include_ignored", false)?;
    let pattern = args
        .get("file_pattern")
        .map(|value| {
            let text = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("file_pattern must be a string"))?;
            Ok::<_, anyhow::Error>(globset::Glob::new(text)?.compile_matcher())
        })
        .transpose()?;
    let needle = if case_sensitive {
        query.to_owned()
    } else {
        query.to_ascii_lowercase()
    };
    let mut builder = ignore::WalkBuilder::new(&start);
    builder
        .follow_links(false)
        .hidden(search && !include_hidden)
        .git_ignore(search && !include_ignored)
        .git_exclude(search && !include_ignored)
        .git_global(false)
        .ignore(false)
        .parents(true)
        .require_git(false)
        .sort_by_file_name(|a, b| a.cmp(b));
    if !search {
        builder.max_depth(Some(1));
    }
    builder.filter_entry(move |entry| {
        let name = entry.file_name().to_string_lossy();
        (!search || name != ".git")
            && (!search
                || include_ignored
                || !entry.file_type().is_some_and(|t| t.is_dir())
                || !matches!(
                    name.as_ref(),
                    "node_modules"
                        | "target"
                        | "dist"
                        | "build"
                        | "vendor"
                        | "__pycache__"
                        | ".venv"
                ))
    });
    let mut results = Vec::new();
    let mut visited = 0;
    let mut bytes_read = 0;
    let mut skipped = 0;
    let mut matched = 0;
    let mut truncated = false;
    let mut scan_limited = false;
    let mut warnings = Vec::new();
    let mut output_bytes = 0;
    let mut collect = |candidate: Value| {
        matched += 1;
        if matched <= offset {
            return false;
        }
        let size = candidate.to_string().len();
        if results.len() == limit || output_bytes + size > 48_000 {
            return true;
        }
        output_bytes += size;
        results.push(candidate);
        false
    };
    'scan: for entry in builder.build() {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        visited += 1;
        if visited > 5000 {
            truncated = true;
            scan_limited = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                skipped += 1;
                if warnings.len() < 10 {
                    warnings.push("An entry could not be inspected; coverage is incomplete.");
                }
                continue;
            }
        };
        if entry.error().is_some() && warnings.len() < 10 {
            warnings
                .push("An ignore file could not be fully parsed; some ignore rules may not apply.");
        }
        let path = entry.path();
        if path == start && start.is_dir() {
            continue;
        }
        let meta = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if !search {
            if collect(json!({"path":paths::display_path(&ctx.root, path),
                "type":if meta.file_type().is_symlink() {"symlink"} else if meta.is_dir() {"directory"} else if meta.is_file() {"file"} else {"other"}, "bytes":meta.len()}))
            {
                truncated = true;
                break;
            }
        } else {
            if meta.is_dir() {
                continue;
            }
            if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 1024 * 1024 {
                skipped += 1;
                continue;
            }
            let relative = paths::display_path(&ctx.root, path);
            if pattern
                .as_ref()
                .is_some_and(|pattern| !pattern.is_match(&relative))
            {
                continue;
            }
            let safe = match paths::confine(&ctx.root, &path.to_string_lossy()) {
                Ok(path) => path,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            let bytes = match paths::read_bounded(&safe, 1024 * 1024) {
                Ok(bytes) => bytes,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
            bytes_read += bytes.len();
            if bytes_read > 16 * 1024 * 1024 {
                truncated = true;
                scan_limited = true;
                break;
            }
            if bytes.len() > 1024 * 1024 || bytes.contains(&0) {
                skipped += 1;
                continue;
            }
            let Ok(text) = std::str::from_utf8(&bytes) else {
                skipped += 1;
                continue;
            };
            for (index, line) in text.lines().enumerate() {
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                let haystack = if case_sensitive {
                    line.to_owned()
                } else {
                    line.to_ascii_lowercase()
                };
                if let Some(position) = haystack.find(&needle) {
                    // Preserve the actual match even in long/minified lines.
                    let before = line[..position].chars().count();
                    let first = before.saturating_sub(80);
                    let excerpt: String = line.chars().skip(first).take(700).collect();
                    if collect(json!({"path":relative, "line":index+1,
                        "text":excerpt, "excerpt_start_column":first+1, "line_truncated":first>0 || line.chars().count()>700}))
                    {
                        truncated = true;
                        break 'scan;
                    }
                }
            }
        }
    }
    Ok(
        json!({"results":results, "truncated":truncated, "scan_limited":scan_limited,
        "next_offset":if truncated && !scan_limited {Some(offset+results.len())} else {None},
        "skipped":skipped, "visited":visited, "bytes_read":bytes_read, "warnings":warnings,
        "coverage":"Bounded scan; hidden, ignored, binary, large, unreadable and symlink entries may be excluded. Narrow path or adjust filters when needed."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn discovers_and_searches_with_limits() {
        let temp = TempDir::new();
        let ctx = ToolContext {
            root: paths::canonical_root(temp.path()).unwrap(),
            allow_shell: false,
            skills: vec![],
            writes: Default::default(),
        };
        std::fs::create_dir(ctx.root.join("notes")).unwrap();
        std::fs::write(
            ctx.root.join("notes/daily.txt"),
            "hello coworker\nhello again",
        )
        .unwrap();
        std::fs::write(ctx.root.join(".hidden"), "hello secret").unwrap();
        let cancel = CancellationToken::new();
        let listing = run(&ctx, &json!({}), false, &cancel).unwrap();
        assert_eq!(listing["results"].as_array().unwrap().len(), 2);
        let found = run(&ctx, &json!({"query":"hello", "limit":1}), true, &cancel).unwrap();
        assert_eq!(found["results"][0]["path"], "notes/daily.txt");
        assert_eq!(found["results"][0]["line"], 1);
        assert_eq!(found["truncated"], true);
        assert!(run(&ctx, &json!({"path":"../outside"}), false, &cancel).is_err());
        assert!(run(&ctx, &json!({"query":""}), true, &cancel).is_err());
        cancel.cancel();
        assert!(run(&ctx, &json!({}), false, &cancel).is_err());
    }

    #[test]
    fn search_filters_ignored_generated_hidden_and_globs_and_pages() {
        let temp = TempDir::new();
        let ctx = ToolContext {
            root: paths::canonical_root(temp.path()).unwrap(),
            allow_shell: false,
            skills: vec![],
            writes: Default::default(),
        };
        std::fs::create_dir_all(ctx.root.join("node_modules")).unwrap();
        std::fs::write(ctx.root.join("node_modules/dependency.rs"), "Needle").unwrap();
        std::fs::write(ctx.root.join(".gitignore"), "ignored.rs").unwrap();
        for file in ["a.rs", "b.rs", "c.txt", "ignored.rs", ".hidden.rs"] {
            std::fs::write(
                ctx.root.join(file),
                format!("{}Needle suffix", "界".repeat(300)),
            )
            .unwrap();
        }
        let token = CancellationToken::new();
        let args =
            json!({"query":"needle", "case_sensitive":false, "file_pattern":"**/*.rs", "limit":1});
        let first = run(&ctx, &args, true, &token).unwrap();
        assert_eq!(first["results"][0]["path"], "a.rs");
        assert!(first["results"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Needle"));
        assert_eq!(first["results"][0]["excerpt_start_column"], 221);
        assert_eq!(first["next_offset"], 1);
        let mut next = args.clone();
        next["offset"] = first["next_offset"].clone();
        let second = run(&ctx, &next, true, &token).unwrap();
        assert_eq!(second["results"][0]["path"], "b.rs");
        assert_eq!(second["truncated"], false);
        let all = run(
            &ctx,
            &json!({"query":"Needle", "include_hidden":true, "include_ignored":true}),
            true,
            &token,
        )
        .unwrap();
        assert_eq!(all["results"].as_array().unwrap().len(), 6);
        assert!(run(
            &ctx,
            &json!({"query":"x", "case_sensitive":"false"}),
            true,
            &token
        )
        .is_err());
        assert!(run(
            &ctx,
            &json!({"query":"x", "file_pattern":"["}),
            true,
            &token
        )
        .is_err());
    }

    #[test]
    fn structured_search_output_stays_parseable_under_registry_cap() {
        let temp = TempDir::new();
        let ctx = ToolContext {
            root: paths::canonical_root(temp.path()).unwrap(),
            allow_shell: false,
            skills: vec![],
            writes: Default::default(),
        };
        std::fs::write(
            ctx.root.join("large.txt"),
            format!("needle{}\n", "\t".repeat(800)).repeat(200),
        )
        .unwrap();
        let result = execute(
            &ctx,
            &json!({"query":"needle", "limit":200}),
            true,
            &CancellationToken::new(),
        );
        let capped = super::super::truncate_output(result.content);
        let value: Value = serde_json::from_str(&capped).unwrap();
        assert_eq!(value["truncated"], true);
        assert!(value["next_offset"].as_u64().unwrap() > 0);
    }

    #[cfg(unix)]
    #[test]
    fn search_skips_symlinks_and_binary_files() {
        let temp = TempDir::new();
        let root = paths::canonical_root(&temp.path().join("root")).unwrap();
        std::fs::write(temp.path().join("outside"), "needle").unwrap();
        std::os::unix::fs::symlink(temp.path().join("outside"), root.join("link")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("cycle")).unwrap();
        std::fs::write(root.join("binary"), b"needle\0").unwrap();
        let ctx = ToolContext {
            root,
            allow_shell: false,
            skills: vec![],
            writes: Default::default(),
        };
        let found = run(
            &ctx,
            &json!({"query":"needle"}),
            true,
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(found["results"], json!([]));
        assert_eq!(found["skipped"], 3);
    }
}
