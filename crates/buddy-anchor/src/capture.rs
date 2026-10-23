//! Content capture for an anchored window.
//!
//! Capture is deliberately separated from the geometry loop: [`enrich`] runs
//! once per user send, reads text from the anchored application, and (for
//! vision requests, or as an OCR fallback) takes a screenshot of that exact
//! window. It never focuses, raises, injects input, reads the clipboard, or
//! changes desktop configuration.
//!
//! Every failure is reported as a warning on `context["content"]` instead of
//! failing the chat. Subprocesses are bounded by `CaptureConfig::timeout_ms`,
//! killed on timeout/cancellation, and their stdout is capped so a misbehaving
//! helper cannot grow memory without bound.
//!
//! ## Screenshot safety
//!
//! Before running `grim`, [`enrich`] takes a fresh Hyprland snapshot and
//! refuses to capture unless the anchored window is still mapped, on the
//! visible workspace, fully inside one enabled monitor, and not covered by a
//! higher-stacked client. A refused screenshot leaves text/OCR untouched: text
//! capture remains useful when compositor screencopy permission or visibility
//! is unavailable.
//!
//! ## Test hooks
//!
//! For tests and local diagnostics the command paths can be overridden with
//! `DROPS_CAPTURE_PYTHON`, `DROPS_CAPTURE_GRIM`, and `DROPS_CAPTURE_TESSERACT`.
//! Production defaults are `/usr/bin/python`, `grim`, and `tesseract`.

use std::process::Stdio;
use std::time::Duration;

use anyhow::Result;
use base64::Engine as _;
use image::ImageFormat;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::hit::{
    client_monitor, client_rect, client_visible, is_buddy_surface, monitor_rect, window_label, Rect,
};
use crate::model::{Client, Snapshot};

/// Text helper source embedded at build time, so the daemon needs no runtime
/// path setup or Python import path for Drops' own script.
const TEXT_HELPER_SOURCE: &str = include_str!("../../../scripts/capture_text.py");

const DEFAULT_TIMEOUT_MS: u64 = 2_500;
const MAX_TIMEOUT_MS: u64 = 60_000;
const MAX_TEXT_CHARS: usize = 1_000_000;
const DEFAULT_IMAGE_EDGE: u32 = 1_280;
const MAX_IMAGE_EDGE: u32 = 8_192;
const MAX_TEXT_HELPER_BYTES: usize = 2 * 1024 * 1024;
const MAX_OCR_BYTES: usize = 2 * 1024 * 1024;
const MAX_SCREENSHOT_BYTES: usize = 32 * 1024 * 1024;
const MAX_ENCODED_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 8 * 1024;
const MAX_IMAGE_PIXELS: u64 = 80_000_000;
const MAX_SOURCES: usize = 8;
const MAX_HELPER_WARNINGS: usize = 12;
const MAX_WARNING_DETAIL: usize = 320;
/// Below this much non-whitespace text, OCR is considered worth trying.
const OCR_MIN_TEXT_CHARS: usize = 32;
const PNG_MAGIC: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Capture configuration for one anchored window.
///
/// Missing JSON fields fall back to [`Default`]; invalid sizes/timeouts are
/// clamped when used rather than rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    /// Master switch. When false, no subprocess is started.
    pub enabled: bool,
    /// Collect accessibility text from the matching application/window.
    pub accessibility: bool,
    /// Collect terminal scrollback through the tmux helper.
    pub terminal: bool,
    /// Allow a screenshot of the anchored window.
    pub screenshots: bool,
    /// OCR the screenshot when accessibility/terminal text is inadequate.
    pub ocr: bool,
    /// Maximum characters returned in `content.text`.
    pub max_text_chars: usize,
    /// Longest image edge after downscaling.
    pub max_image_edge: u32,
    /// Per-command timeout. Timeouts become warnings, never chat failures.
    pub timeout_ms: u64,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        CaptureConfig {
            enabled: true,
            accessibility: true,
            terminal: true,
            screenshots: true,
            ocr: true,
            max_text_chars: 24_000,
            max_image_edge: DEFAULT_IMAGE_EDGE,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

impl CaptureConfig {
    fn timeout(&self) -> Duration {
        let ms = if self.timeout_ms == 0 {
            DEFAULT_TIMEOUT_MS
        } else {
            self.timeout_ms.min(MAX_TIMEOUT_MS)
        };
        Duration::from_millis(ms)
    }

    fn text_limit(&self) -> usize {
        self.max_text_chars.min(MAX_TEXT_CHARS)
    }

    fn image_edge(&self) -> u32 {
        if self.max_image_edge == 0 {
            DEFAULT_IMAGE_EDGE
        } else {
            self.max_image_edge.clamp(16, MAX_IMAGE_EDGE)
        }
    }

    fn text_helper_enabled(&self) -> bool {
        self.accessibility || self.terminal
    }
}

/// Enrich a window context bundle with text, a vision image, and warnings.
///
/// Non-window or malformed contexts are left untouched: workspace anchors and
/// headless callers have no anchored window to read. Operational failures
/// (missing tools, compositor consent, hidden or covered windows, timeouts)
/// are recorded on `context["content"]["warnings"]` and `enrich` still returns
/// `Ok(())` so a chat send is never aborted by capture.
///
/// The optional image is attached only when `vision` is true and screenshots
/// are enabled. OCR still runs with `vision == false`; the image itself is
/// discarded so text-only models never receive base64 bytes.
pub async fn enrich(
    context: &mut Value,
    config: &CaptureConfig,
    vision: bool,
    cancel: CancellationToken,
) -> Result<()> {
    let Some(kind) = context.get("kind").and_then(Value::as_str) else {
        return Ok(());
    };
    if kind != "window" {
        return Ok(());
    }

    let Some(target) = WindowTarget::from_context(context) else {
        set_content(
            context,
            unavailable_content(
                Vec::new(),
                vec![warning(
                    "capture_unavailable",
                    "context has no usable anchored window address",
                )],
                None,
            ),
        );
        return Ok(());
    };

    if !config.enabled {
        set_content(
            context,
            content_value(
                Status::Disabled,
                Vec::new(),
                String::new(),
                Vec::new(),
                None,
            ),
        );
        return Ok(());
    }

    if cancel.is_cancelled() {
        set_content(
            context,
            unavailable_content(
                Vec::new(),
                vec![warning(
                    "capture_cancelled",
                    "capture was cancelled before it started",
                )],
                None,
            ),
        );
        return Ok(());
    }

    // Lock safety comes before text capture: accessibility can otherwise read
    // a lock surface beneath the session lock. When the context flags a locked
    // screen, require a fresh compositor confirmation before starting any
    // helper.
    let context_locked = context
        .get("screen_locked")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let needs_snapshot = config.screenshots || context_locked;
    let snapshot_result: std::result::Result<Option<Snapshot>, String> = if needs_snapshot {
        fresh_snapshot(config, &cancel).await.map(Some)
    } else {
        Ok(None)
    };
    if let Some(detail) = lock_refusal(context_locked, &snapshot_result) {
        set_content(context, locked_content(detail));
        return Ok(());
    }

    // The helper receives the ordinary context bundle, minus any stale capture
    // result, and only ever sees the window that was dropped on.
    let helper_input = if config.text_helper_enabled() {
        let mut input = context.clone();
        if let Some(map) = input.as_object_mut() {
            map.remove("content");
            // Helper-side feature flags prevent disabled sources from being
            // read at all, instead of filtering them after collection.
            map.insert(
                "capture_options".to_string(),
                json!({
                    "accessibility": config.accessibility,
                    "terminal": config.terminal,
                    "max_text_chars": config.text_limit(),
                }),
            );
        }
        serde_json::to_vec(&input).ok()
    } else {
        None
    };

    let helper_future = async {
        match helper_input {
            Some(input) => capture_text(&input, config, &cancel).await,
            None => TextCapture::default(),
        }
    };
    let text_capture = helper_future.await;

    let mut warnings = text_capture.warnings;
    if text_capture.cancelled {
        warnings.push(warning(
            "capture_cancelled",
            "text capture was cancelled; partial content may be missing",
        ));
    }

    let limit = config.text_limit();
    let mut text = String::new();
    let mut sources = Vec::new();
    let mut text_truncated = false;

    for source in &text_capture.sources {
        let sanitized = sanitize_text(&source.text);
        if sanitized.trim().is_empty() {
            continue;
        }

        let separator = if text.is_empty() { 0 } else { 2 };
        let remaining = limit.saturating_sub(text.chars().count() + separator);
        if remaining == 0 {
            text_truncated = true;
            sources.push(source_meta(&source.kind, &source.scope, 0, true));
            continue;
        }

        let (chunk, cut) = clamp_chars(&sanitized, remaining);
        let truncated = source.truncated || cut;
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&chunk);
        text_truncated |= truncated;
        sources.push(source_meta(
            &source.kind,
            &source.scope,
            chunk.chars().count(),
            truncated,
        ));
    }

    let mut screenshot_png = None;
    match snapshot_result {
        Ok(Some(snapshot)) => match plan_visible_rect(&snapshot, &target) {
            Ok(rect) => match grab_screenshot(rect, config, &cancel).await {
                Ok(png) => {
                    screenshot_png = Some(png);
                    sources.push(source_meta("screenshot", "visible-window", 0, false));
                }
                Err(detail) => {
                    if cancel.is_cancelled() {
                        warnings.push(warning(
                            "capture_cancelled",
                            "screenshot capture was cancelled",
                        ));
                    } else {
                        warnings.push(warning("screenshot_unavailable", detail));
                    }
                }
            },
            Err(problem) => warnings.push(problem.warning()),
        },
        Ok(None) => {}
        Err(detail) => {
            if !cancel.is_cancelled() {
                warnings.push(warning(
                    "screenshot_unavailable",
                    format!("could not verify the window is visible: {detail}"),
                ));
            }
        }
    }

    // OCR is a fallback: accessibility/tmux text wins whenever it is useful.
    if let Some(png) = &screenshot_png {
        if config.ocr && text_is_inadequate(&text) && !cancel.is_cancelled() {
            match run_ocr(png, limit, config.timeout(), &cancel).await {
                Ok(ocr) => {
                    if !ocr.text.is_empty() && !text.contains(ocr.text.trim()) {
                        let separator = if text.is_empty() { 0 } else { 2 };
                        let remaining = limit.saturating_sub(text.chars().count() + separator);
                        if remaining == 0 {
                            text_truncated = true;
                        } else {
                            if !text.is_empty() {
                                text.push_str("\n\n");
                            }
                            let (chunk, cut) = clamp_chars(&ocr.text, remaining);
                            text.push_str(&chunk);
                            text_truncated |= cut;
                            sources.push(source_meta(
                                "ocr",
                                "visible-window",
                                chunk.chars().count(),
                                cut || ocr.truncated,
                            ));
                        }
                    }
                }
                Err(detail) => {
                    if !cancel.is_cancelled() {
                        warnings.push(warning("ocr_unavailable", detail));
                    }
                }
            }
        }
    }

    if text_truncated {
        warnings.push(warning(
            "text_truncated",
            format!("captured text was limited to {limit} characters"),
        ));
    }

    let mut image = None;
    if should_attach_image(vision, config) {
        if let Some(png) = &screenshot_png {
            match prepare_image(png, config.image_edge()) {
                Ok((encoded, width, height)) => {
                    let data_url = format!(
                        "data:image/png;base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(encoded)
                    );
                    image = Some(json!({
                        "mime_type": "image/png",
                        "width": width,
                        "height": height,
                        "data_url": data_url,
                    }));
                }
                Err(detail) => warnings.push(warning("image_unavailable", detail)),
            }
        }
    }

    let has_text = !text.trim().is_empty();
    let status = if has_text || image.is_some() {
        Status::Captured
    } else {
        Status::Unavailable
    };
    if status == Status::Unavailable && warnings.is_empty() {
        warnings.push(warning(
            "capture_unavailable",
            "no accessible text, terminal output, image, or OCR text was found for the anchored window",
        ));
    }

    set_content(
        context,
        content_value(status, sources, text, warnings, image),
    );
    Ok(())
}

fn set_content(context: &mut Value, content: Value) {
    if let Some(map) = context.as_object_mut() {
        map.insert("content".to_string(), content);
    }
}

fn unavailable_content(sources: Vec<Value>, warnings: Vec<String>, image: Option<Value>) -> Value {
    content_value(Status::Unavailable, sources, String::new(), warnings, image)
}

fn content_value(
    status: Status,
    sources: Vec<Value>,
    text: String,
    warnings: Vec<String>,
    image: Option<Value>,
) -> Value {
    let mut content = json!({
        "status": status.as_str(),
        "sources": sources,
        "text": text,
        "warnings": warnings,
    });
    if let Some(image) = image {
        content["image"] = image;
    }
    content
}

fn source_meta(kind: &str, scope: &str, chars: usize, truncated: bool) -> Value {
    json!({
        "kind": kind,
        "scope": scope,
        "chars": chars,
        "truncated": truncated,
    })
}

fn should_attach_image(vision: bool, config: &CaptureConfig) -> bool {
    vision && config.screenshots
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Captured,
    Unavailable,
    Disabled,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Captured => "captured",
            Status::Unavailable => "unavailable",
            Status::Disabled => "disabled",
        }
    }
}

fn warning(code: &str, detail: impl AsRef<str>) -> String {
    format!("{code}: {}", clean_detail(detail.as_ref()))
}

fn clean_detail(detail: &str) -> String {
    let single_line = detail
        .replace(['\r', '\n'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    clamp_chars(&single_line, MAX_WARNING_DETAIL).0
}

fn clamp_chars(text: &str, limit: usize) -> (String, bool) {
    let mut out = String::new();
    for (index, character) in text.chars().enumerate() {
        if index >= limit {
            return (out, true);
        }
        out.push(character);
    }
    (out, false)
}

fn sanitize_text(text: &str) -> String {
    text.chars()
        .filter_map(|character| match character {
            '\0' => None,
            '\r' => Some('\n'),
            '\n' | '\t' => Some(character),
            other if other.is_control() => None,
            other => Some(other),
        })
        .collect()
}

fn text_is_inadequate(text: &str) -> bool {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .count()
        < OCR_MIN_TEXT_CHARS
}

struct WindowTarget {
    address: String,
    pid: i32,
    class: String,
}

impl WindowTarget {
    fn from_context(context: &Value) -> Option<Self> {
        let window = context.get("window")?;
        let address = window
            .get("address")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|address| !address.is_empty())?
            .to_string();
        let pid = window
            .get("pid")
            .and_then(Value::as_i64)
            .and_then(|pid| i32::try_from(pid).ok())
            .unwrap_or(0);
        let class = window
            .get("class")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Some(WindowTarget {
            address,
            pid,
            class,
        })
    }
}

#[derive(Debug)]
enum RectProblem {
    Missing,
    Replaced,
    Hidden,
    OffMonitor,
    Occluded(String),
}

impl RectProblem {
    fn warning(&self) -> String {
        match self {
            RectProblem::Missing => warning(
                "capture_unavailable",
                "the anchored window is no longer open",
            ),
            RectProblem::Replaced => warning(
                "capture_unavailable",
                "the anchored window address now belongs to another process; refusing to capture it",
            ),
            RectProblem::Hidden => warning(
                "screenshot_unavailable",
                "the anchored window is hidden or on an inactive workspace; bring it into view and send again",
            ),
            RectProblem::OffMonitor => warning(
                "screenshot_unavailable",
                "the anchored window is partially off its monitor or off-screen; move it fully into view",
            ),
            RectProblem::Occluded(label) => warning(
                "screenshot_unavailable",
                format!("the anchored window is covered by \"{label}\"; move it or switch to it before sending"),
            ),
        }
    }
}

/// Resolve the exact rectangle that may be captured, or explain why not.
///
/// This is pure so window visibility, monitor containment, and stacking can be
/// tested from recorded snapshots without a compositor.
fn plan_visible_rect(snapshot: &Snapshot, target: &WindowTarget) -> Result<Rect, RectProblem> {
    let needle = crate::normalize_address(&target.address);
    let client = snapshot
        .clients
        .iter()
        .find(|client| crate::normalize_address(&client.address) == needle)
        .ok_or(RectProblem::Missing)?;

    if target.pid > 0 && client.pid != target.pid {
        return Err(RectProblem::Replaced);
    }
    if !target.class.trim().is_empty()
        && !client.class.eq_ignore_ascii_case(target.class.trim())
        && !client
            .initial_class
            .eq_ignore_ascii_case(target.class.trim())
    {
        return Err(RectProblem::Replaced);
    }
    if is_buddy_surface(client) {
        return Err(RectProblem::Missing);
    }
    if !client_visible(snapshot, client) {
        return Err(RectProblem::Hidden);
    }

    let monitor = client_monitor(snapshot, client).ok_or(RectProblem::OffMonitor)?;
    if !monitor.is_enabled() {
        return Err(RectProblem::OffMonitor);
    }

    let rect = client_rect(client);
    if rect.width <= 0 || rect.height <= 0 || !contains_rect(monitor_rect(monitor), rect) {
        return Err(RectProblem::OffMonitor);
    }

    if let Some(label) = occluding_window(snapshot, client, rect) {
        return Err(RectProblem::Occluded(label));
    }

    Ok(rect)
}

/// The topmost visible client that covers any part of `rect`, if any.
///
/// Windows on other monitors, Drops' own surfaces, hidden clients, and clients
/// below the target in the stack are ignored. A fullscreen client hides every
/// non-fullscreen window on its monitor.
fn occluding_window(snapshot: &Snapshot, target: &Client, rect: Rect) -> Option<String> {
    let monitor = client_monitor(snapshot, target)?;
    let target_rank = stack_rank(target);
    let target_fullscreen = target.is_fullscreen();

    for other in &snapshot.clients {
        if crate::normalize_address(&other.address) == crate::normalize_address(&target.address) {
            continue;
        }
        if is_buddy_surface(other) || !client_visible(snapshot, other) {
            continue;
        }
        if client_monitor(snapshot, other).map(|monitor| monitor.id) != Some(monitor.id) {
            continue;
        }
        if intersection_area(rect, client_rect(other)) == 0 {
            continue;
        }

        let covers = if other.is_fullscreen() {
            true
        } else if target_fullscreen {
            false
        } else {
            stack_rank(other) < target_rank
        };
        if covers {
            return Some(clean_detail(&window_label(other)));
        }
    }
    None
}

fn stack_rank(client: &Client) -> (u8, u32, String) {
    let layer = if client.floating || client.pinned {
        0
    } else {
        1
    };
    let focus = if client.focus_history_id < 0 {
        u32::MAX
    } else {
        client.focus_history_id as u32
    };
    (layer, focus, crate::normalize_address(&client.address))
}

fn contains_rect(outer: Rect, inner: Rect) -> bool {
    if inner.width <= 0 || inner.height <= 0 {
        return false;
    }
    let left = inner.x as i64;
    let top = inner.y as i64;
    let right = left + inner.width as i64;
    let bottom = top + inner.height as i64;
    left >= outer.x as i64
        && top >= outer.y as i64
        && right <= outer.x as i64 + outer.width as i64
        && bottom <= outer.y as i64 + outer.height as i64
}

fn intersection_area(left: Rect, right: Rect) -> i64 {
    let x1 = left.x.max(right.x) as i64;
    let y1 = left.y.max(right.y) as i64;
    let x2 = (left.x as i64 + left.width as i64).min(right.x as i64 + right.width as i64);
    let y2 = (left.y as i64 + left.height as i64).min(right.y as i64 + right.height as i64);
    (x2 - x1).max(0) * (y2 - y1).max(0)
}

async fn fresh_snapshot(
    config: &CaptureConfig,
    cancel: &CancellationToken,
) -> std::result::Result<Snapshot, String> {
    tokio::select! {
        _ = cancel.cancelled() => Err("cancelled".to_string()),
        result = tokio::time::timeout(config.timeout(), crate::snapshot()) => {
            match result {
                Ok(Ok(snapshot)) => Ok(snapshot),
                Ok(Err(error)) => Err(format!("Hyprland query failed ({error})")),
                Err(_) => Err("Hyprland query timed out".to_string()),
            }
        }
    }
}

/// True when the compositor reports a session lock on any monitor.
///
/// Hyprland exposes this as `solitaryBlockedBy: ["LOCK"]` on monitors. Lock
/// surfaces are not clients, so client occlusion alone cannot detect a locked
/// screen.
fn snapshot_is_locked(snapshot: &Snapshot) -> bool {
    snapshot.monitors.iter().any(|monitor| {
        monitor
            .solitary_blocked_by
            .iter()
            .any(|reason| reason.trim().eq_ignore_ascii_case("LOCK"))
    })
}

/// Decide whether capture must stop before any helper or screenshot starts.
///
/// The context's `screen_locked` flag is only trusted after a fresh snapshot
/// confirms the compositor lock. A fresh `LOCK` flag always refuses capture,
/// even when the context is stale. When the context says locked and the fresh
/// check fails, capture fails closed instead of reading beneath the lock.
fn lock_refusal(
    context_locked: bool,
    snapshot_result: &std::result::Result<Option<Snapshot>, String>,
) -> Option<String> {
    match snapshot_result {
        Ok(Some(snapshot)) if snapshot_is_locked(snapshot) => {
            Some("the compositor reports a locked session (monitor LOCK flag)".to_string())
        }
        Ok(Some(_)) => None,
        Ok(None) if context_locked => Some(
            "the context reports a locked screen and no fresh compositor snapshot was available"
                .to_string(),
        ),
        Err(detail) if context_locked => Some(format!(
            "the context reports a locked screen and the fresh compositor check failed ({detail})"
        )),
        _ => None,
    }
}

fn locked_content(detail: impl AsRef<str>) -> Value {
    unavailable_content(
        Vec::new(),
        vec![warning(
            "screen_locked",
            format!(
                "{}; refusing to read windows while locked; unlock the desktop and send again",
                detail.as_ref()
            ),
        )],
        None,
    )
}

fn command_path(env_name: &str, default: &str) -> String {
    std::env::var(env_name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn python_program() -> String {
    command_path("DROPS_CAPTURE_PYTHON", "/usr/bin/python")
}

fn grim_program() -> String {
    command_path("DROPS_CAPTURE_GRIM", "grim")
}

fn tesseract_program() -> String {
    command_path("DROPS_CAPTURE_TESSERACT", "tesseract")
}

#[derive(Default)]
struct TextCapture {
    sources: Vec<TextSource>,
    warnings: Vec<String>,
    cancelled: bool,
}

struct TextSource {
    kind: String,
    scope: String,
    text: String,
    truncated: bool,
}

#[derive(Default, Deserialize)]
struct RawHelper {
    #[serde(default)]
    sources: Vec<RawSource>,
    #[serde(default)]
    warnings: Vec<Value>,
}

#[derive(Default, Deserialize)]
struct RawSource {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    text: String,
}

async fn capture_text(
    input: &[u8],
    config: &CaptureConfig,
    cancel: &CancellationToken,
) -> TextCapture {
    let mut stdin = input.to_vec();
    stdin.push(b'\n');

    let mut command = Command::new(python_program());
    command
        .arg("-B")
        .arg("-c")
        .arg(TEXT_HELPER_SOURCE)
        .env("PYTHONDONTWRITEBYTECODE", "1");
    let output = run_bounded(
        command,
        Some(stdin),
        MAX_TEXT_HELPER_BYTES,
        config.timeout(),
        cancel,
    )
    .await;

    if output.cancelled {
        return TextCapture {
            cancelled: true,
            ..TextCapture::default()
        };
    }
    decode_text_helper(&output, config)
}

fn decode_text_helper(output: &CommandOutcome, config: &CaptureConfig) -> TextCapture {
    if output.timed_out {
        return TextCapture {
            warnings: vec![warning(
                "text_helper_timeout",
                "the text helper did not finish within the capture timeout; screenshots may still work",
            )],
            ..TextCapture::default()
        };
    }
    if let Some(error) = &output.error {
        return TextCapture {
            warnings: vec![warning(
                "text_helper_failed",
                format!("could not run the text helper ({error})"),
            )],
            ..TextCapture::default()
        };
    }
    if !output.success {
        let detail = if output.stderr.trim().is_empty() {
            "the text helper exited unsuccessfully".to_string()
        } else {
            format!("the text helper failed: {}", output.stderr.trim())
        };
        return TextCapture {
            warnings: vec![warning("text_helper_failed", detail)],
            ..TextCapture::default()
        };
    }

    let raw = match parse_helper_output(&output.stdout) {
        Ok(raw) => raw,
        Err(detail) => {
            return TextCapture {
                warnings: vec![warning("text_helper_failed", detail)],
                ..TextCapture::default()
            }
        }
    };

    let mut capture = TextCapture::default();
    if output.truncated {
        capture.warnings.push(warning(
            "text_helper_truncated",
            "the text helper produced more output than the capture limit",
        ));
    }
    capture.warnings.extend(helper_warnings(&raw));

    for source in raw.sources.into_iter().take(MAX_SOURCES) {
        let kind = source.kind.trim().to_ascii_lowercase();
        match kind.as_str() {
            "accessibility" => {
                if !config.accessibility {
                    continue;
                }
            }
            "tmux" => {
                if !config.terminal {
                    continue;
                }
            }
            _ => {
                capture.warnings.push(warning(
                    "text_helper_source_ignored",
                    format!(
                        "unknown text source kind \"{}\"",
                        clean_detail(&source.kind)
                    ),
                ));
                continue;
            }
        }

        let scope = if source.scope.trim().is_empty() {
            "matched-window".to_string()
        } else {
            clamp_chars(&clean_detail(&source.scope), 64).0
        };
        // Clamp before sanitizing so a huge helper payload cannot allocate
        // much more than the configured text budget. The truncation flag is
        // preserved so provenance still reports lossy capture.
        let (raw_text, truncated) = clamp_chars(&source.text, config.text_limit().max(1));
        capture.sources.push(TextSource {
            kind,
            scope,
            text: raw_text,
            truncated,
        });
    }

    capture
}

fn parse_helper_output(bytes: &[u8]) -> std::result::Result<RawHelper, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "the text helper returned invalid UTF-8".to_string())?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("the text helper returned no output".to_string());
    }
    if let Ok(parsed) = serde_json::from_str::<RawHelper>(trimmed) {
        return Ok(parsed);
    }
    for line in trimmed.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(parsed) = serde_json::from_str::<RawHelper>(line) {
            return Ok(parsed);
        }
    }
    Err("could not parse the text helper JSON output".to_string())
}

fn helper_warnings(raw: &RawHelper) -> Vec<String> {
    raw.warnings
        .iter()
        .filter_map(Value::as_str)
        .take(MAX_HELPER_WARNINGS)
        .map(|message| warning("text_helper", message))
        .collect()
}

async fn grab_screenshot(
    rect: Rect,
    config: &CaptureConfig,
    cancel: &CancellationToken,
) -> std::result::Result<Vec<u8>, String> {
    let mut command = Command::new(grim_program());
    command
        .arg("-g")
        .arg(format!(
            "{},{} {}x{}",
            rect.x, rect.y, rect.width, rect.height
        ))
        .arg("-t")
        .arg("png")
        .arg("-");

    let output = run_bounded(
        command,
        None,
        MAX_SCREENSHOT_BYTES,
        config.timeout(),
        cancel,
    )
    .await;
    if output.cancelled {
        return Err("cancelled".to_string());
    }
    if output.timed_out {
        return Err("grim timed out while capturing the window".to_string());
    }
    if let Some(error) = output.error {
        return Err(format!(
            "could not run grim ({error}); install grim or check the session PATH"
        ));
    }
    if !output.success {
        let detail = if output.stderr.trim().is_empty() {
            "grim exited unsuccessfully".to_string()
        } else {
            format!("grim failed: {}", output.stderr.trim())
        };
        return Err(format!(
            "{detail}; Hyprland may require screencopy permission for Drops"
        ));
    }
    if output.stdout.is_empty() {
        return Err("grim returned an empty screenshot".to_string());
    }
    if output.truncated {
        return Err("the screenshot exceeded the capture size limit".to_string());
    }
    if !output.stdout.starts_with(PNG_MAGIC) {
        return Err("grim did not return a PNG image".to_string());
    }
    Ok(output.stdout)
}

struct OcrText {
    text: String,
    truncated: bool,
}

async fn run_ocr(
    png: &[u8],
    limit: usize,
    timeout: Duration,
    cancel: &CancellationToken,
) -> std::result::Result<OcrText, String> {
    let mut command = Command::new(tesseract_program());
    command.args(["stdin", "stdout", "--psm", "6"]);
    let output = run_bounded(command, Some(png.to_vec()), MAX_OCR_BYTES, timeout, cancel).await;
    if output.cancelled {
        return Err("cancelled".to_string());
    }
    if output.timed_out {
        return Err("tesseract timed out while reading the window".to_string());
    }
    if let Some(error) = output.error {
        return Err(format!(
            "tesseract is not available ({error}); accessibility or tmux text may still work"
        ));
    }
    if !output.success {
        let detail = if output.stderr.trim().is_empty() {
            "tesseract exited unsuccessfully".to_string()
        } else {
            format!("tesseract failed: {}", output.stderr.trim())
        };
        return Err(detail);
    }
    if output.truncated {
        return Err("OCR output exceeded the capture size limit".to_string());
    }

    let raw = sanitize_text(&String::from_utf8_lossy(&output.stdout));
    let (text, truncated) = clamp_chars(raw.trim(), limit);
    Ok(OcrText { text, truncated })
}

/// Validate and downscale a PNG before it is attached to the context.
fn prepare_image(png: &[u8], max_edge: u32) -> std::result::Result<(Vec<u8>, u32, u32), String> {
    png_dimensions(png).ok_or_else(|| {
        "the screenshot was not a valid PNG within the decode size limit".to_string()
    })?;

    let decoded = image::load_from_memory_with_format(png, ImageFormat::Png)
        .map_err(|error| format!("could not decode the screenshot ({error})"))?;
    let (width, height) = (decoded.width(), decoded.height());
    if width == 0 || height == 0 {
        return Err("the screenshot has no pixels".to_string());
    }

    let (target_width, target_height) =
        fit_within(width, height, max_edge.clamp(16, MAX_IMAGE_EDGE));
    let resized = if (target_width, target_height) == (width, height) {
        decoded
    } else {
        decoded.resize_exact(
            target_width,
            target_height,
            image::imageops::FilterType::Triangle,
        )
    };

    let mut encoded = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut encoded);
    resized
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(|error| format!("could not encode the downscaled screenshot ({error})"))?;
    if encoded.len() > MAX_ENCODED_IMAGE_BYTES {
        return Err("the downscaled screenshot exceeded the image size limit".to_string());
    }
    Ok((encoded, target_width, target_height))
}

fn png_dimensions(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != PNG_MAGIC {
        return None;
    }
    let width = u32::from_be_bytes([png[16], png[17], png[18], png[19]]);
    let height = u32::from_be_bytes([png[20], png[21], png[22], png[23]]);
    if width == 0 || height == 0 {
        return None;
    }
    if width.max(height) > 20_000 || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return None;
    }
    Some((width, height))
}

fn fit_within(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= max_edge {
        return (width, height);
    }
    let scale = f64::from(max_edge) / f64::from(longest);
    (
        (f64::from(width) * scale).round().max(1.0) as u32,
        (f64::from(height) * scale).round().max(1.0) as u32,
    )
}

#[derive(Default)]
struct CommandOutcome {
    stdout: Vec<u8>,
    stderr: String,
    success: bool,
    timed_out: bool,
    truncated: bool,
    cancelled: bool,
    error: Option<String>,
}

/// Run a command with bounded stdin/stdout/stderr and a hard timeout.
///
/// The child is killed on timeout, cancellation, and drop. Reads continue to
/// drain after the cap so a chatty process cannot deadlock on a full pipe; only
/// the first `stdout_limit` bytes are retained.
async fn run_bounded(
    mut command: Command,
    stdin_data: Option<Vec<u8>>,
    stdout_limit: usize,
    timeout: Duration,
    cancel: &CancellationToken,
) -> CommandOutcome {
    command
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CommandOutcome {
                error: Some(error.to_string()),
                ..CommandOutcome::default()
            }
        }
    };

    let writer = match (child.stdin.take(), stdin_data) {
        (Some(mut stdin), Some(data)) => Some(tokio::spawn(async move {
            let _ = stdin.write_all(&data).await;
            let _ = stdin.shutdown().await;
        })),
        _ => None,
    };

    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.start_kill();
        return CommandOutcome {
            error: Some("could not read subprocess output".to_string()),
            ..CommandOutcome::default()
        };
    };

    let read = async {
        tokio::join!(
            read_capped(stdout, stdout_limit),
            read_capped(stderr, MAX_STDERR_BYTES),
        )
    };

    enum Phase {
        Cancelled,
        TimedOut,
        Read((Vec<u8>, bool), (Vec<u8>, bool)),
    }

    let phase = tokio::select! {
        _ = cancel.cancelled() => Phase::Cancelled,
        result = tokio::time::timeout(timeout, read) => {
            match result {
                Ok((stdout, stderr)) => Phase::Read(stdout, stderr),
                Err(_) => Phase::TimedOut,
            }
        }
    };

    let mut outcome = CommandOutcome::default();
    match phase {
        Phase::Read((stdout, stdout_truncated), (stderr, _)) => {
            outcome.stdout = stdout;
            outcome.truncated = stdout_truncated;
            outcome.stderr = String::from_utf8_lossy(&stderr).trim().to_string();
            match tokio::time::timeout(Duration::from_millis(1_000), child.wait()).await {
                Ok(Ok(status)) => outcome.success = status.success(),
                Ok(Err(error)) => outcome.error = Some(error.to_string()),
                Err(_) => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    outcome.timed_out = true;
                }
            }
        }
        Phase::TimedOut => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
            outcome.timed_out = true;
        }
        Phase::Cancelled => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
            outcome.cancelled = true;
        }
    }

    if let Some(writer) = writer {
        writer.abort();
    }
    outcome
}

async fn read_capped<R: AsyncRead + Unpin>(mut reader: R, limit: usize) -> (Vec<u8>, bool) {
    let mut retained = Vec::new();
    let mut truncated = false;
    let mut buffer = [0u8; 8 * 1024];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                if retained.len() < limit {
                    let take = (limit - retained.len()).min(read);
                    retained.extend_from_slice(&buffer[..take]);
                    if take < read {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    (retained, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        serde_json::from_str(include_str!("../tests/fixtures/hyprland-two-monitors.json"))
            .expect("fixture")
    }

    fn target(address: &str, pid: i32, class: &str) -> WindowTarget {
        WindowTarget {
            address: address.to_string(),
            pid,
            class: class.to_string(),
        }
    }

    #[test]
    fn config_defaults_match_the_contract() {
        let config = CaptureConfig::default();
        assert!(config.enabled);
        assert!(config.accessibility);
        assert!(config.terminal);
        assert!(config.screenshots);
        assert!(config.ocr);
        assert_eq!(config.max_text_chars, 24_000);
        assert_eq!(config.max_image_edge, 1_280);
        assert_eq!(config.timeout_ms, 2_500);
    }

    #[test]
    fn partial_config_uses_defaults_and_clamps() {
        let mut config: CaptureConfig =
            serde_json::from_str(r#"{"enabled":false,"max_image_edge":0,"timeout_ms":0}"#)
                .expect("partial config");
        assert!(!config.enabled);
        assert!(config.accessibility);
        assert_eq!(config.image_edge(), DEFAULT_IMAGE_EDGE);
        assert_eq!(config.timeout(), Duration::from_millis(DEFAULT_TIMEOUT_MS));

        config.timeout_ms = 10;
        assert_eq!(config.timeout(), Duration::from_millis(10));
        config.max_image_edge = 99_999;
        assert_eq!(config.image_edge(), MAX_IMAGE_EDGE);
    }

    #[test]
    fn text_clamping_never_splits_a_character() {
        let (clamped, cut) = clamp_chars("héllo", 2);
        assert_eq!(clamped, "hé");
        assert!(cut);

        let (clamped, cut) = clamp_chars("hello", 10);
        assert_eq!(clamped, "hello");
        assert!(!cut);
    }

    #[test]
    fn helper_json_is_parsed_and_sources_filtered_by_config() {
        let output = CommandOutcome {
            stdout: br#"{
                "sources": [
                    {"kind": "accessibility", "scope": "matched-window", "text": "a11y text"},
                    {"kind": "tmux", "scope": "terminal-scrollback", "text": "tmux text"},
                    {"kind": "mystery", "scope": "x", "text": "ignored"}
                ],
                "warnings": ["helper warning"]
            }"#
            .to_vec(),
            success: true,
            ..CommandOutcome::default()
        };

        let config = CaptureConfig {
            terminal: false,
            ..CaptureConfig::default()
        };
        let capture = decode_text_helper(&output, &config);
        assert_eq!(capture.sources.len(), 1);
        assert_eq!(capture.sources[0].kind, "accessibility");
        assert_eq!(capture.sources[0].text, "a11y text");
        assert!(capture
            .warnings
            .iter()
            .any(|warning| warning.starts_with("text_helper:")));
        assert!(capture
            .warnings
            .iter()
            .any(|warning| warning.starts_with("text_helper_source_ignored:")));
    }

    #[test]
    fn helper_parses_the_last_json_line_but_rejects_garbage() {
        let last_line = parse_helper_output(b"starting helper\n{\"sources\":[],\"warnings\":[]}\n")
            .expect("json line");
        assert!(last_line.sources.is_empty());

        assert!(parse_helper_output(b"not json").is_err());
        assert!(parse_helper_output(b"").is_err());
    }

    #[test]
    fn helper_failure_becomes_a_warning() {
        let output = CommandOutcome {
            success: false,
            stderr: "AT-SPI unavailable".to_string(),
            ..CommandOutcome::default()
        };
        let capture = decode_text_helper(&output, &CaptureConfig::default());
        assert!(capture.sources.is_empty());
        assert!(capture.warnings[0].starts_with("text_helper_failed:"));
        assert!(capture.warnings[0].contains("AT-SPI unavailable"));
    }

    #[test]
    fn visible_unoccluded_window_yields_its_rect() {
        let rect = plan_visible_rect(&snapshot(), &target("0x2001", 221, "firefox"))
            .expect("visible window");
        assert_eq!(rect, Rect::new(1920, 0, 1920, 1080));
    }

    #[test]
    fn hidden_and_inactive_workspace_windows_are_refused() {
        let snapshot = snapshot();
        assert!(matches!(
            plan_visible_rect(&snapshot, &target("0x1003", 114, "ghost")),
            Err(RectProblem::Hidden)
        ));
        assert!(matches!(
            plan_visible_rect(&snapshot, &target("0x3001", 331, "code")),
            Err(RectProblem::Hidden)
        ));
    }

    #[test]
    fn occluded_window_is_refused_instead_of_captured() {
        // The floating Firefox window covers the terminal on the same monitor.
        let error = plan_visible_rect(&snapshot(), &target("0x1001", 111, "Alacritty"))
            .expect_err("covered window");
        match error {
            RectProblem::Occluded(label) => assert!(label.contains("Docs"), "{label}"),
            other => panic!("expected occlusion, got {other:?}"),
        }
    }

    #[test]
    fn windows_above_the_occluder_are_allowed() {
        let mut snapshot = snapshot();
        let terminal = snapshot
            .clients
            .iter_mut()
            .find(|client| client.address == "0x1001")
            .expect("terminal");
        terminal.floating = true;
        terminal.focus_history_id = 0;

        let rect = plan_visible_rect(&snapshot, &target("0x1001", 111, "Alacritty"))
            .expect("focused floating window is on top");
        assert_eq!(rect, Rect::new(0, 0, 1000, 500));
    }

    #[test]
    fn partially_off_monitor_windows_are_refused() {
        let mut snapshot = snapshot();
        let terminal = snapshot
            .clients
            .iter_mut()
            .find(|client| client.address == "0x1001")
            .expect("terminal");
        terminal.at = [1900, 0];
        terminal.size = [200, 100];

        assert!(matches!(
            plan_visible_rect(&snapshot, &target("0x1001", 111, "Alacritty")),
            Err(RectProblem::OffMonitor)
        ));
    }

    #[test]
    fn replaced_or_missing_windows_are_refused() {
        let snapshot = snapshot();
        assert!(matches!(
            plan_visible_rect(&snapshot, &target("0x2001", 999, "firefox")),
            Err(RectProblem::Replaced)
        ));
        assert!(matches!(
            plan_visible_rect(&snapshot, &target("0xdead", 0, "")),
            Err(RectProblem::Missing)
        ));
    }

    #[test]
    fn fit_within_preserves_aspect_ratio() {
        assert_eq!(fit_within(1920, 1080, 1280), (1280, 720));
        assert_eq!(fit_within(400, 300, 1280), (400, 300));
        assert_eq!(fit_within(1, 10_000, 1_280), (1, 1280));
    }

    #[test]
    fn png_dimensions_reject_oversized_headers() {
        let mut header = Vec::from(*PNG_MAGIC);
        header.extend_from_slice(&[0; 8]);
        header.extend_from_slice(&100_000u32.to_be_bytes());
        header.extend_from_slice(&100_000u32.to_be_bytes());
        assert_eq!(png_dimensions(&header), None);
        assert_eq!(png_dimensions(b"not png"), None);
    }

    #[test]
    fn screenshot_is_downscaled_and_reencoded() {
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            64,
            32,
            image::Rgba([12, 34, 56, 255]),
        ));
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png)
            .expect("encode fixture");

        let (encoded, width, height) = prepare_image(&png, 16).expect("prepare image");
        assert_eq!((width, height), (16, 8));
        assert!(encoded.starts_with(PNG_MAGIC));
        assert_eq!(png_dimensions(&encoded), Some((16, 8)));
        assert!(encoded.len() < png.len() || encoded.len() < 4096);
    }

    #[test]
    fn image_is_only_attached_for_vision_requests() {
        let config = CaptureConfig::default();
        assert!(should_attach_image(true, &config));
        assert!(!should_attach_image(false, &config));

        let no_screenshots = CaptureConfig {
            screenshots: false,
            ..CaptureConfig::default()
        };
        assert!(!should_attach_image(true, &no_screenshots));
    }

    #[test]
    fn ocr_runs_only_when_text_is_inadequate() {
        assert!(text_is_inadequate(""));
        assert!(text_is_inadequate("prompt $"));
        assert!(!text_is_inadequate(
            "a terminal with enough captured output to answer the question"
        ));
    }

    #[test]
    fn locked_snapshot_is_detected_from_monitor_flags() {
        let mut snapshot = snapshot();
        assert!(!snapshot_is_locked(&snapshot));
        snapshot.monitors[0]
            .solitary_blocked_by
            .push("LOCK".to_string());
        assert!(snapshot_is_locked(&snapshot));
        snapshot.monitors[0].solitary_blocked_by = vec!["lock".to_string()];
        assert!(snapshot_is_locked(&snapshot));
    }

    #[test]
    fn lock_guard_fails_closed_and_accepts_a_confirmed_unlock() {
        let unlocked = snapshot();
        let unavailable: std::result::Result<Option<Snapshot>, String> =
            Err("headless".to_string());

        assert!(lock_refusal(false, &Ok(Some(unlocked.clone()))).is_none());
        assert!(lock_refusal(true, &Ok(Some(unlocked.clone()))).is_none());
        assert!(lock_refusal(true, &unavailable).is_some());
        assert!(lock_refusal(false, &unavailable).is_none());
        assert!(lock_refusal(true, &Ok(None)).is_some());

        let mut locked = unlocked;
        locked.monitors[0]
            .solitary_blocked_by
            .push("LOCK".to_string());
        let refusal = lock_refusal(false, &Ok(Some(locked))).expect("locked snapshot");
        assert!(refusal.contains("LOCK"));
    }

    #[test]
    fn intersection_and_containment_handle_monitor_origins() {
        let monitor = Rect::new(1920, 0, 1920, 1080);
        assert!(contains_rect(monitor, Rect::new(1920, 0, 1920, 1080)));
        assert!(!contains_rect(monitor, Rect::new(1900, 0, 100, 100)));
        assert_eq!(
            intersection_area(Rect::new(0, 0, 10, 10), Rect::new(5, 5, 10, 10)),
            25
        );
        assert_eq!(
            intersection_area(Rect::new(0, 0, 10, 10), Rect::new(10, 0, 10, 10)),
            0
        );
    }

    fn mock_dir(label: &str) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "buddy-capture-unit-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn executable(path: &std::path::Path, source: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, source).expect("write script");
        let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod script");
    }

    #[test]
    fn helper_text_truncation_is_preserved_in_provenance() {
        let output = CommandOutcome {
            stdout: br#"{"sources":[{"kind":"accessibility","text":"more text than allowed"}]}"#
                .to_vec(),
            success: true,
            ..CommandOutcome::default()
        };
        let config = CaptureConfig {
            max_text_chars: 4,
            ..CaptureConfig::default()
        };
        let capture = decode_text_helper(&output, &config);
        assert_eq!(capture.sources[0].text, "more");
        assert!(capture.sources[0].truncated);
    }

    #[tokio::test]
    async fn mock_grim_and_tesseract_use_bounded_stdout_pipelines() {
        let dir = mock_dir("commands");
        let png = {
            let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
                8,
                8,
                image::Rgba([1, 2, 3, 255]),
            ));
            let mut bytes = Vec::new();
            image
                .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
                .expect("encode fixture");
            bytes
        };
        let png_path = dir.join("fixture.png");
        std::fs::write(&png_path, &png).expect("write png");

        let args_path = dir.join("grim-args.txt");
        let ocr_path = dir.join("ocr-input.png");
        let grim = dir.join("mock-grim");
        let tesseract = dir.join("mock-tesseract");
        executable(
            &grim,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"{}\"\nexec /bin/cat \"{}\"\n",
                args_path.display(),
                png_path.display()
            ),
        );
        executable(
            &tesseract,
            &format!(
                "#!/bin/sh\n/bin/cat > \"{}\"\nprintf 'ocr fixture text\\n'\n",
                ocr_path.display()
            ),
        );

        std::env::set_var("DROPS_CAPTURE_GRIM", &grim);
        std::env::set_var("DROPS_CAPTURE_TESSERACT", &tesseract);

        let config = CaptureConfig::default();
        let cancel = CancellationToken::new();
        let shot = grab_screenshot(Rect::new(1, 2, 30, 40), &config, &cancel)
            .await
            .expect("mock screenshot");
        assert_eq!(shot, png);

        let args = std::fs::read_to_string(&args_path).expect("grim args");
        assert!(args.lines().any(|line| line == "1,2 30x40"), "{args}");

        let ocr = run_ocr(&shot, 100, config.timeout(), &cancel)
            .await
            .expect("mock ocr");
        assert_eq!(ocr.text, "ocr fixture text");
        let ocr_input = std::fs::read(&ocr_path).expect("ocr stdin");
        assert!(ocr_input.starts_with(PNG_MAGIC));

        std::env::remove_var("DROPS_CAPTURE_GRIM");
        std::env::remove_var("DROPS_CAPTURE_TESSERACT");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn bounded_command_times_out_and_is_killed() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf started; sleep 10"]);
        let output = run_bounded(
            command,
            None,
            1024,
            Duration::from_millis(100),
            &CancellationToken::new(),
        )
        .await;
        assert!(output.timed_out);
        assert!(!output.cancelled);
    }

    #[tokio::test]
    async fn cancelled_command_is_killed_and_reported() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 10"]);
        let output = run_bounded(command, None, 1024, Duration::from_secs(5), &cancel).await;
        assert!(output.cancelled);
        assert!(!output.timed_out);
    }
}
