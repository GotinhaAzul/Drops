//! Evidence-based working context, scoped to the anchored process tree.
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::procfs::ProcessInfo;

pub fn is_terminal(class: &str) -> bool {
    matches!(
        class.to_ascii_lowercase().as_str(),
        "alacritty"
            | "kitty"
            | "foot"
            | "footclient"
            | "com.mitchellh.ghostty"
            | "ghostty"
            | "org.wezfurlong.wezterm"
            | "wezterm"
    )
}
fn is_editor(class: &str) -> bool {
    matches!(
        class.to_ascii_lowercase().as_str(),
        "code"
            | "code-oss"
            | "vscodium"
            | "cursor"
            | "windsurf"
            | "zed"
            | "dev.zed.zed"
            | "jetbrains-idea"
            | "jetbrains-pycharm"
            | "jetbrains-rustrover"
            | "jetbrains-webstorm"
    )
}

fn is_office(class: &str) -> bool {
    let class = class.to_ascii_lowercase();
    [
        "libreoffice",
        "soffice",
        "onlyoffice",
        "desktopeditors",
        "winword",
        "excel",
        "wps",
        "et.exe",
    ]
    .iter()
    .any(|name| class.contains(name))
}

pub async fn detect_document(class: &str, title: &str, process: &ProcessInfo) -> Option<Value> {
    if !is_office(class) {
        return None;
    }
    Some(
        tokio::time::timeout(
            Duration::from_millis(1200),
            document_at(title, process, Path::new("/proc")),
        )
        .await
        .unwrap_or_else(
            |_| json!({"status":"unavailable","warnings":["Local document discovery timed out"]}),
        ),
    )
}

fn document_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| matches!(s.to_ascii_lowercase().as_str(), "docx" | "xlsx"))
}
async fn document_at(title: &str, process: &ProcessInfo, proc_root: &Path) -> Value {
    let mut candidates = BTreeSet::new();
    for arg in process.cmdline.iter().skip(1) {
        let path = PathBuf::from(arg);
        let path = if path.is_absolute() {
            path
        } else if let Some(cwd) = &process.cwd {
            Path::new(cwd).join(path)
        } else {
            continue;
        };
        if document_path(&path) {
            if let Ok(path) = tokio::fs::canonicalize(path).await {
                if path.is_file() {
                    candidates.insert(path);
                }
            }
        }
    }
    // Open handles provide evidence for reused GUI processes. Read link names
    // only, never arbitrary handle contents or unrelated process handles.
    let (pids, _) = descendants(proc_root, process.pid).await;
    let mut scanned = 0;
    'scan: for pid in pids {
        let Ok(mut entries) = tokio::fs::read_dir(proc_root.join(format!("{pid}/fd"))).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            scanned += 1;
            if scanned > 2048 {
                break 'scan;
            }
            let Ok(path) = tokio::fs::read_link(entry.path()).await else {
                continue;
            };
            if document_path(&path) {
                if let Ok(path) = tokio::fs::canonicalize(path).await {
                    if path.is_file() {
                        candidates.insert(path);
                    }
                }
            }
        }
    }
    let matching: BTreeSet<_> = candidates
        .iter()
        .filter(|path| {
            let title = title.to_lowercase();
            let filename = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            title.contains(&filename)
                || (stem.len() >= 3 && (title == stem || title.starts_with(&(stem + " - "))))
        })
        .cloned()
        .collect();
    // Titles only disambiguate paths already verified from args/open handles.
    let selected = if matching.len() == 1 {
        matching.into_iter().next()
    } else if title.is_empty() && candidates.len() == 1 {
        candidates.iter().next().cloned()
    } else {
        None
    };
    if let Some(path) = selected {
        let parent = path.parent().unwrap().to_path_buf();
        let mut context =
            resolve_candidates(BTreeSet::from([parent]), "local-document", None).await;
        context["document"] = json!({"path":path,"format":path.extension().and_then(|s|s.to_str()).unwrap_or("").to_ascii_lowercase(),
            "state":"saved-on-disk", "note":"Unsaved application edits are not included. Document edits produce a new copy; open that copy in the app to review."});
        context
    } else {
        json!({"status":if candidates.is_empty(){"unavailable"}else{"ambiguous"},"source":"local-document", "warnings":["No unique saved DOCX/XLSX file could be tied to this window. Unsaved, browser-hosted and other formats are not supported."],"candidates":candidates.into_iter().take(8).collect::<Vec<_>>()})
    }
}

pub async fn detect(class: &str, process: &ProcessInfo, shared_pid: bool) -> Value {
    let operation = detect_at(class, process, shared_pid, Path::new("/proc"));
    tokio::time::timeout(Duration::from_millis(1800), operation)
        .await
        .unwrap_or_else(
            |_| json!({"status":"unavailable","warnings":["Workspace discovery timed out"]}),
        )
}

async fn detect_at(
    class: &str,
    process: &ProcessInfo,
    shared_pid: bool,
    proc_root: &Path,
) -> Value {
    if !is_terminal(class) && !is_editor(class) {
        return json!({"status":"unavailable","warnings":["This application has no workspace resolver"]});
    }
    if shared_pid {
        return json!({"status":"ambiguous","warnings":["Several windows share this application PID; workspace selection cannot be tied to this window"]});
    }
    let mut candidates = BTreeSet::new();
    let mut source = "editor-arguments";
    let mut active_file = None;
    if is_terminal(class) {
        let (pids, complete) = descendants(proc_root, process.pid).await;
        if !complete {
            return json!({"status":"ambiguous","warnings":["Process tree was incomplete or exceeded its scan limit"]});
        }
        let mut shells = BTreeSet::new();
        let mut has_tmux = false;
        for pid in &pids {
            let base = proc_root.join(pid.to_string());
            let Ok(bytes) = tokio::fs::read(base.join("cmdline")).await else {
                continue;
            };
            let args = crate::procfs::parse_cmdline(&bytes);
            let name = args
                .first()
                .and_then(|s| Path::new(s).file_name())
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .trim_start_matches('-');
            has_tmux |= name == "tmux";
            let Ok(cwd) = tokio::fs::read_link(base.join("cwd")).await else {
                continue;
            };
            let Ok(cwd) = tokio::fs::canonicalize(cwd).await else {
                continue;
            };
            if matches!(name, "bash" | "zsh" | "fish" | "sh" | "dash" | "nu") {
                shells.insert(cwd.clone());
            }
            if let Ok(stat) = tokio::fs::read_to_string(base.join("stat")).await {
                if foreground(&stat) {
                    candidates.insert(cwd);
                }
            }
        }
        if has_tmux && proc_root == Path::new("/proc") {
            match tmux_workspace(&pids).await {
                Some(path) => {
                    candidates.clear();
                    candidates.insert(path);
                    source = "tmux-active-pane";
                }
                None => {
                    return json!({"status":"ambiguous","warnings":["tmux detected, but its active pane could not be uniquely matched to this window"]})
                }
            }
        } else {
            source = "terminal-foreground-process";
            if candidates.is_empty() {
                candidates = shells;
                source = "terminal-shell";
            }
        }
    } else {
        let cwd = process.cwd.as_deref().map(Path::new);
        let mut skip_next = false;
        for arg in process.cmdline.iter().skip(1) {
            if skip_next {
                skip_next = false;
                continue;
            }
            if arg.starts_with('-') {
                // Only explicit directory flags are meaningful workspace evidence.
                if !matches!(
                    arg.as_str(),
                    "--folder-uri"
                        | "--file-uri"
                        | "--project"
                        | "--reuse-window"
                        | "--new-window"
                        | "-n"
                        | "-r"
                        | "--wait"
                        | "-w"
                        | "--goto"
                        | "-g"
                ) {
                    skip_next = true;
                }
                continue;
            }
            let path = if let Some(path) = arg.strip_prefix("file://") {
                PathBuf::from(path)
            } else {
                PathBuf::from(arg)
            };
            let path = if path.is_absolute() {
                path
            } else if let Some(cwd) = cwd {
                cwd.join(path)
            } else {
                continue;
            };
            let Ok(path) = tokio::fs::canonicalize(path).await else {
                continue;
            };
            if path.is_dir() {
                candidates.insert(path);
            } else if path.is_file() {
                active_file = Some(path.clone());
                if let Some(parent) = path.parent() {
                    candidates.insert(parent.to_path_buf());
                }
            }
        }
    }
    resolve_candidates(candidates, source, active_file).await
}

// stat begins with pid (comm with spaces/parentheses), then state, ppid,
// pgrp, session, tty_nr, tpgid. Only foreground processes with a tty qualify.
fn foreground(stat: &str) -> bool {
    let Some((_, tail)) = stat.rsplit_once(')') else {
        return false;
    };
    let fields: Vec<_> = tail.split_whitespace().collect();
    let number = |index| fields.get(index).and_then(|s: &&str| s.parse::<i64>().ok());
    matches!((number(2),number(4),number(5)), (Some(group),Some(tty),Some(front)) if tty != 0 && group > 0 && group == front)
}

async fn descendants(root: &Path, pid: i32) -> (HashSet<i32>, bool) {
    let mut seen = HashSet::new();
    let mut pending = vec![pid];
    while let Some(pid) = pending.pop() {
        if pid <= 0 || !seen.insert(pid) {
            continue;
        }
        if seen.len() > 256 {
            return (seen, false);
        }
        let path = root.join(format!("{pid}/task/{pid}/children"));
        let Ok(children) = tokio::fs::read_to_string(path).await else {
            return (seen, false);
        };
        pending.extend(
            children
                .split_whitespace()
                .filter_map(|s| s.parse::<i32>().ok()),
        );
    }
    (seen, true)
}

async fn tmux(args: &[&str]) -> Option<String> {
    let mut command = tokio::process::Command::new("tmux");
    command.args(args).env_remove("TMUX").kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_millis(450), command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() || output.stdout.len() > 64000 {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}
async fn tmux_workspace(pids: &HashSet<i32>) -> Option<PathBuf> {
    let listing = tmux(&["list-clients", "-F", "#{client_pid}\t#{client_tty}"]).await?;
    let matches: Vec<_> = listing
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(pid, _)| {
            pid.parse::<i32>()
                .ok()
                .is_some_and(|pid| pids.contains(&pid))
        })
        .collect();
    if matches.len() != 1 {
        return None;
    }
    let path = tmux(&[
        "display-message",
        "-p",
        "-c",
        matches[0].1,
        "#{pane_current_path}",
    ])
    .await?;
    if path.lines().count() != 1 {
        return None;
    }
    tokio::fs::canonicalize(path.trim_end_matches('\n'))
        .await
        .ok()
}

async fn repository(path: &Path) -> Option<PathBuf> {
    for ancestor in path.ancestors() {
        if ancestor.parent().is_none() {
            break;
        }
        if tokio::fs::symlink_metadata(ancestor.join(".git"))
            .await
            .is_ok()
        {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}
async fn resolve_candidates(
    candidates: BTreeSet<PathBuf>,
    source: &str,
    active_file: Option<PathBuf>,
) -> Value {
    // Several editor files within one repo are one workspace; multiple terminal
    // directories remain ambiguous even if they happen to share a repository.
    let mut roots = BTreeSet::new();
    for candidate in &candidates {
        let root = repository(candidate)
            .await
            .unwrap_or_else(|| candidate.clone());
        roots.insert(root);
    }
    if candidates.is_empty()
        || roots.len() != 1
        || (source.starts_with("terminal") && candidates.len() != 1)
    {
        return json!({"status":if candidates.is_empty(){"unavailable"}else{"ambiguous"},"source":source,"candidates":candidates.into_iter().take(8).collect::<Vec<_>>(),"warnings":["No unique working directory could be established; no directory was guessed from the window title"]});
    }
    let root = roots.into_iter().next().unwrap();
    let cwd = candidates.into_iter().next().unwrap();
    let repo = repository(&cwd).await;
    let mut project_files = Vec::new();
    for name in [
        "Cargo.toml",
        "package.json",
        "pyproject.toml",
        "go.mod",
        "pom.xml",
        "build.gradle",
        "CMakeLists.txt",
        "Makefile",
        "README.md",
    ] {
        if tokio::fs::metadata(root.join(name))
            .await
            .is_ok_and(|meta| meta.is_file())
        {
            project_files.push(name);
        }
    }

    json!({"status":"detected", "source":source, "cwd":cwd, "root":root,
        "repository":repo.map(|root|json!({"root":root,"name":root.file_name().map(|n|n.to_string_lossy().into_owned())})),
        "opened_file":active_file, "project_files":project_files, "warnings":[]})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "drops-workspace-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn stat_parsing_handles_spaces_and_parentheses() {
        assert!(foreground("10 (a tricky ) name) S 1 10 10 42 10 0"));
        assert!(!foreground("10 (bash) S 1 10 10 42 20 0"));
        assert!(!foreground("10 (bash) S 1 10 10 0 10 0"));
    }
    #[tokio::test]
    async fn editor_paths_find_repository_and_ambiguity() {
        let base = temp();
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: /unused/worktree").unwrap();
        std::fs::write(repo.join("src/main.rs"), "hello").unwrap();
        let process = ProcessInfo {
            pid: 10,
            cwd: Some(base.to_string_lossy().into_owned()),
            cmdline: vec!["zed".into(), "repo/src/main.rs".into()],
            available: true,
            error: None,
        };
        let found = detect_at("dev.zed.Zed", &process, false, &base).await;
        assert_eq!(found["status"], "detected");
        assert_eq!(found["root"], repo.to_string_lossy().as_ref());
        assert_eq!(
            found["opened_file"],
            repo.join("src/main.rs").to_string_lossy().as_ref()
        );
        assert_eq!(
            detect_at("zed", &process, true, &base).await["status"],
            "ambiguous"
        );
        let mut candidates = BTreeSet::from([repo]);
        candidates.insert(base.clone());
        assert_eq!(
            resolve_candidates(candidates, "editor-arguments", None).await["status"],
            "ambiguous"
        );
        std::fs::remove_dir_all(base).unwrap();
    }
    #[tokio::test]
    async fn office_handles_require_a_matching_title_and_never_guess_from_titles() {
        let base = temp();
        let proc = base.join("proc");
        std::fs::create_dir_all(proc.join("10/task/10")).unwrap();
        std::fs::create_dir_all(proc.join("10/fd")).unwrap();
        std::fs::write(proc.join("10/task/10/children"), "").unwrap();
        let first = base.join("report.docx");
        let second = base.join("budget.xlsx");
        std::fs::write(&first, "fixture").unwrap();
        std::fs::write(&second, "fixture").unwrap();
        std::os::unix::fs::symlink(&first, proc.join("10/fd/3")).unwrap();
        std::os::unix::fs::symlink(&second, proc.join("10/fd/4")).unwrap();
        let process = ProcessInfo {
            pid: 10,
            ..Default::default()
        };
        let found = document_at("report.docx - LibreOffice Writer", &process, &proc).await;
        assert_eq!(found["document"]["path"], first.to_string_lossy().as_ref());
        assert_eq!(found["document"]["state"], "saved-on-disk");
        assert_eq!(
            document_at("Untitled document", &process, &proc).await["status"],
            "ambiguous"
        );
        assert_eq!(
            document_at(
                "report.docx",
                &ProcessInfo {
                    pid: 11,
                    ..Default::default()
                },
                &proc
            )
            .await["status"],
            "unavailable"
        );
        std::fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn discovers_a_real_shell_working_directory_from_proc() {
        use std::process::Stdio;
        let base = temp();
        std::fs::create_dir(base.join(".git")).unwrap();
        std::fs::write(base.join("Cargo.toml"), "[workspace]").unwrap();
        let mut shell = tokio::process::Command::new("bash")
            .args(["--noprofile", "--norc"])
            .current_dir(&base)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = shell.id().unwrap() as i32;
        let found = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let process = crate::procfs::process_info(pid).await;
                let found = detect("kitty", &process, false).await;
                if found["status"] == "detected" {
                    break found;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        shell.kill().await.unwrap();
        shell.wait().await.unwrap();
        let found = found.unwrap();
        assert_eq!(found["cwd"], base.to_string_lossy().as_ref());
        assert_eq!(found["repository"]["root"], base.to_string_lossy().as_ref());
        assert_eq!(found["project_files"], json!(["Cargo.toml"]));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn terminal_child_shell_cwd_updates_and_tabs_are_ambiguous() {
        let base = temp();
        let proc = base.join("proc");
        let a = base.join("a");
        let b = base.join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        for (pid, children, name, cwd) in [(10, "11", "kitty", &base), (11, "", "bash", &a)] {
            std::fs::create_dir_all(proc.join(format!("{pid}/task/{pid}"))).unwrap();
            std::fs::write(proc.join(format!("{pid}/task/{pid}/children")), children).unwrap();
            std::fs::write(proc.join(format!("{pid}/cmdline")), format!("{name}\0")).unwrap();
            std::os::unix::fs::symlink(cwd, proc.join(format!("{pid}/cwd"))).unwrap();
        }
        let process = ProcessInfo {
            pid: 10,
            ..Default::default()
        };
        assert_eq!(
            detect_at("kitty", &process, false, &proc).await["cwd"],
            a.to_string_lossy().as_ref()
        );
        std::fs::remove_file(proc.join("11/cwd")).unwrap();
        std::os::unix::fs::symlink(&b, proc.join("11/cwd")).unwrap();
        assert_eq!(
            detect_at("kitty", &process, false, &proc).await["cwd"],
            b.to_string_lossy().as_ref()
        );
        std::fs::write(proc.join("10/task/10/children"), "11 12").unwrap();
        std::fs::create_dir_all(proc.join("12/task/12")).unwrap();
        std::fs::write(proc.join("12/task/12/children"), "").unwrap();
        std::fs::write(proc.join("12/cmdline"), "zsh\0").unwrap();
        std::os::unix::fs::symlink(&a, proc.join("12/cwd")).unwrap();
        assert_eq!(
            detect_at("kitty", &process, false, &proc).await["status"],
            "ambiguous"
        );
        std::fs::remove_dir_all(base).unwrap();
    }
}
