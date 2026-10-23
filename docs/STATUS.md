# Implementation and validation status

This is the initial Drops build. The feature inventory below separates code from checks that require an actual desktop or model endpoint.

## Implemented

- Six Rust workspace crates following the spec's boundaries.
- OpenAI-compatible Chat Completions, SSE text/reasoning/usage and fragmented indexed tool calls, omitted tool IDs and non-streaming fallback.
- Agent continuation, malformed-call recovery, project-scoped file read/write/edit, gated shell and external stdio tools, markdown instructions/skills, JSONL sessions.
- NDJSON Unix socket daemon, private socket, CLI/Waybar state, persistent blob metadata, up to three independent sessions, clear/export/fork, per-tool approvals and cancellation.
- Hyprland queries, hit testing, window/workspace anchors, reconnecting socket2 updates, persisted decoration offsets and context with window geometry and process metadata.
- Native Omarchy top-bar widget with explicit create/delete controls, per-drop Drop/Cat/Sprout appearances, endpoint/model setup, and smooth pointer-following drag animations.
- Minimal QML panel: avatar and chat field, closes on placement; placed drops open contextual conversations. Stable surface identity during streaming, exact approval cards, context inspection via the anchor label, and a stop control.
- Thinking-model reasoning is shown as one collapsed, expandable block with a bounded scroll area; `[appearance].thinking_mode` hides it or expands it by default.
- TOML provider selection, anchor rules and external executable declarations; local install script, one-click launcher and example Waybar configuration.

## Executed verification

- 239 Rust tests pass, plus 34 Python text-capture checks. Capture covers lock refusal, geometry/occlusion, bounded subprocesses, image gating, multimodal recovery and state scrubbing.
- Workspace Clippy passes with `-D warnings`; formatting is clean.
- Failure-path integration verifies same-bridge cancellation during slow capture, pending-session fences, disk-save recovery, startup anchor refresh, retained offsets, graceful external-tool shutdown and strict-endpoint continuation after interrupted calls.
- Headless integration passes streaming tool continuation, exact approval gating, denial, cancellation during approval, busy-session guards, file changes, context inspection, export/fork/clear, blob cap, malformed IPC, restart persistence and socket permissions.
- Live Hyprland integration passes real window attachment, connected UI load, workspace hide/return, floating-window movement, and automatic deletion when an attached window closes, using only a disposable Foot terminal.
- Offscreen populated QML checks validate controls and preserve chat delegates and blob surface identity during streaming. Additional interaction checks exercise real create/style/delete controls, endpoint form submission, close-on-drop and non-instant drag interpolation. A reasoning-specific check verifies the collapsed default, bounded expansion, and zero-height hiding.
- Widget integration tests cover first-run setup without a config, endpoint validation and private persistence, appearance persistence, draft placement, and deletion while a turn is awaiting approval.
- Release installer was executed into an isolated prefix with private config permissions, executable example tool and working CLI. The installed one-click launcher starts the daemon/UI once and toggles the panel on successive clicks. The native widget was also enabled in the user Omarchy bar, preserving the existing layout.

## Validation limits

Real Ollama/llama-server/hosted SSE recordings are not claimed: provider fixtures are synthetic compatibility fixtures. Two live endpoint implementations must still be checked to close milestone 1 exactly as written.

The video session was restored during implementation. The actual `ui/shell.qml` then loaded successfully on Hyprland with the Wayland backend (`Configuration Loaded`, no runtime QML errors). Offscreen checks also load the controls using a substitute window surface. A disposable Foot terminal also passed live hit-testing, socket2 workspace hide/return and window-automatic deletion when an attached window closes, with the UI connected. Actual pointer dragging, multi-monitor mapping and fractional scaling still require interactive validation.

Window content capture now uses matched-window accessibility, optional matching tmux scrollback, visible-window OCR and vision attachments. Live disposable terminal, GTK editor and Chromium page checks passed through a local mock endpoint. Complete terminal scrollback and full page/document text depend on accessibility or tmux support; OCR covers only the visible crop. Locked sessions refuse capture. Colors, size and roundness are configured in `[appearance]`; automatic Omarchy theme tracking remains follow-up work. Per-tool permission overrides and richer anchor-scoped tool lists also remain follow-up work; the initial policy is deliberately fixed: core file mutations and shell commands require approval.

A five-second debug-build sample with one real window drop measured 0.20% of one CPU core and 13.4 MiB daemon RSS. This excludes Quickshell memory and is not a full performance benchmark. Window geometry is checked every 200 ms through the control socket only while window drops exist; workspace-only sessions do no geometry IPC. The five-minute onboarding and ten-minute external-tool onboarding targets have not been measured.
