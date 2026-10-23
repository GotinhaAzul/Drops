//! Process metadata for anchored windows, read from `/proc`.
//!
//! Only the PID Hyprland reports for the anchored window is inspected. The
//! returned bundle contains the process's working directory and command line,
//! as promised by the product context contract.

use serde::Serialize;

/// Working directory and command line of an anchored window's process.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProcessInfo {
    pub pid: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub cmdline: Vec<String>,
    /// False when `/proc/<pid>` could not be inspected at all.
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ProcessInfo {
    fn unavailable(pid: i32, error: impl Into<String>) -> Self {
        ProcessInfo {
            pid,
            cwd: None,
            cmdline: Vec::new(),
            available: false,
            error: Some(error.into()),
        }
    }
}

/// Read `/proc/<pid>/cwd` and `/proc/<pid>/cmdline`.
///
/// This never fails the whole context bundle: a process that has already
/// exited or is not readable yields `available: false` with an explanation.
pub async fn process_info(pid: i32) -> ProcessInfo {
    if pid <= 0 {
        return ProcessInfo::unavailable(pid, "window did not report a process id");
    }

    let cwd = match tokio::fs::read_link(format!("/proc/{pid}/cwd")).await {
        Ok(path) => Some(path.to_string_lossy().into_owned()),
        Err(_) => None,
    };

    let cmdline = match tokio::fs::read(format!("/proc/{pid}/cmdline")).await {
        Ok(bytes) => parse_cmdline(&bytes),
        Err(error) => {
            return ProcessInfo {
                pid,
                cwd,
                cmdline: Vec::new(),
                available: false,
                error: Some(format!("could not read /proc/{pid}/cmdline: {error}")),
            };
        }
    };

    ProcessInfo {
        pid,
        cwd,
        cmdline,
        available: true,
        error: None,
    }
}

/// Split a NUL-separated `/proc/<pid>/cmdline` payload.
pub fn parse_cmdline(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| String::from_utf8_lossy(argument).into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nul_separated_cmdline() {
        assert_eq!(
            parse_cmdline(b"bash\0-c\0echo hi\0"),
            vec!["bash", "-c", "echo hi"]
        );
    }

    #[test]
    fn tolerates_trailing_and_empty_segments() {
        assert_eq!(
            parse_cmdline(b"kitty\0\0--single\0"),
            vec!["kitty", "--single"]
        );
        assert!(parse_cmdline(b"").is_empty());
        assert!(parse_cmdline(b"\0\0\0").is_empty());
    }

    #[test]
    fn replaces_invalid_utf8() {
        let parsed = parse_cmdline(b"ok\0bad\xffarg\0");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], "ok");
        assert!(parsed[1].starts_with("bad"));
        assert!(parsed[1].ends_with("arg"));
    }

    #[test]
    fn non_positive_pid_is_unavailable() {
        let info = ProcessInfo::unavailable(0, "no pid");
        assert!(!info.available);
        assert!(info.cwd.is_none());
    }
}
