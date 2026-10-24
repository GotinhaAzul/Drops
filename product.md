# Buddy: Product Description

*A hackable desktop companion for Omarchy. Working title; draft v0.1.*

## 1. One-line summary

Buddy is a small, customizable blob that lives on your Linux desktop. You place it on top of a window or workspace, tell it what to do in plain language, and an AI agent acts with full awareness of what the blob is sitting on.

## 2. The problem

Most AI assistants live in a browser tab or a terminal pane, disconnected from what you are actually looking at. To get help with a window, you copy text, take a screenshot, paste it into a chat, explain the situation, and then carry the answer back by hand. The assistant never knows which window you mean, which workspace you are in, or what is running behind the pixels.

Tiling-WM users in particular work across many windows and workspaces at once, and they value tools that are fast, keyboard- and mouse-light, themeable, and fully under their own control. Existing assistants are heavyweight, closed, cloud-first, and impossible to reshape.

## 3. The vision

An assistant should be *placed*, not *summoned into a void*. Dragging a blob onto a window is a direct, physical way of saying "this is what I mean." The act of placing is the context. No copying, no describing, no explaining which window.

Buddy aims to feel like a small pet you leave on your terminal, your browser, or an empty workspace, each with its own memory and its own job, and to be open enough that anyone can reshape how it behaves.

## 4. Target users

- **Primary:** Omarchy / Hyprland users who are comfortable editing config files and want an assistant that fits their setup.
- **Secondary:** Linux tinkerers running local models (Ollama, llama.cpp, vLLM, LM Studio) who want an agent that works fully offline.
- **Tertiary:** developers who want to write their own tools, anchor adapters, and skills and share them.

## 5. Core concepts

**The dot.** A tiny indicator on the top bar (native Omarchy shell widget; optional Waybar adapter). It shows Buddy's state at a glance and opens the Buddy panel on click.

**The panel.** A small overlay window containing a blob and a chat bar. It is the home base where you start a conversation and where blobs are picked up.

**The blob.** The visual avatar of an agent instance. It can be dragged out of the panel and dropped anywhere on the desktop. Blobs are customizable in color, shape, size, and personality.

**The anchor.** What a blob is attached to: a specific window, a workspace, or the bare desktop. The anchor defines what the agent can see and which tools it has.

**The session.** Each blob has its own conversation and history. One blob on your terminal and another on your browser are two independent agents with separate context.

## 6. User experience

### 6.1 The dot and the panel

The dot sits in the top bar. Its color or animation reflects state: idle, thinking, waiting for your confirmation, or error. Clicking it opens the panel near the bar; clicking again, or pressing Escape, closes it.

The panel is deliberately small. It holds one blob, a chat bar, and a short message history. It can also list the blobs that are already placed, so you can jump to or recall any of them.

### 6.2 Placing a blob

You drag the blob out of the panel and across the screen. While dragging, Buddy highlights whatever is under the cursor (a window outline, or a workspace indicator) so you can see what it will attach to before you let go. On drop, the blob settles on that target with a short animation.

If you drop it on empty desktop space, it becomes a workspace blob that stays on that workspace. This suits Omarchy setups where each workspace has its own fullscreen app, such as a browser on one and a terminal on another. If you drop it on a window, it becomes a window blob and follows that window.

### 6.3 Living on the desktop

A placed blob stays put:

- It follows its window when the window moves, resizes, floats, or changes workspace.
- It hides when its workspace is not visible and reappears when you return.
- When its attached window closes, the drop and its local conversation are deleted, stopping any running turn first.
- It stays small and unobtrusive, and it never steals focus unless you interact with it.

Clicking a placed blob opens a compact chat bar attached to it, so you talk to that blob in context.

### 6.4 Talking to it

You type a message, for example "summarize the error in this terminal" or "clean up this workspace and tell me what's open." The blob shows what it is doing as it works: streaming text, tool activity, and any steps that need your approval. Replies appear in the chat bar, with an optional bubble near the blob.

### 6.5 Approvals

Before a risky action, the blob pauses and shows exactly what it intends to do. You approve or deny with one click or key. Safe, read-only actions run without interruption.

## 7. Context capture

When a blob is anchored, Buddy builds a context bundle for the agent. Which parts are included depends on the target and on user settings.

**Always (cheap, text):**

- Window class, title, and PID
- Workspace and monitor
- Floating or tiled state, and size and position
- For anchored windows: the process's working directory and command line

**Optionally (richer):**

- A screenshot of the target window or workspace, downscaled, when the configured model supports vision
- Adapter-provided data, such as the current directory of a terminal's shell, the URL of a browser tab, or the file open in an editor
- A list of other windows on the workspace, for workspace blobs

The bundle is refreshed when the user sends a message, not continuously, so the agent sees current state without constant overhead. Users can see exactly what was attached to each request.

## 8. What the agent can do

### 8.1 Built-in tools

A deliberately small core, in the spirit of minimal agent harnesses:

- **read:** read files.
- **write / edit:** create and modify files.
- **bash:** run shell commands.
- **window and workspace queries:** inspect what is open.
- **screenshot:** capture the current anchor on demand.

### 8.2 Anchor-specific tools

Tools can exist only while a blob is anchored to a matching target. A terminal anchor might expose tools to read recent output or run commands in that shell's directory. A browser anchor might expose the current URL and page text. A workspace anchor might expose window management actions.

### 8.3 Risk levels

Every tool is classified as read-only, mutating, or dangerous. Read-only runs automatically, mutating requires approval by default, and dangerous always requires explicit confirmation. Users can tune these defaults per anchor type and per tool.

## 9. Customization and extensibility

Extensibility is a first-class goal: Buddy should be easy to modify without recompiling.

- **Appearance:** blob shape, colors, size, animations, and the panel's look, all themeable. Buddy can follow the Omarchy theme automatically.
- **Personality and instructions:** layered markdown context files (global, per anchor type, per project) control behavior and tone.
- **Anchor rules (TOML):** match windows by class, title, or command line, then attach prompts, context, and tools.
- **External tools:** any executable that speaks a simple JSON protocol over stdio can be a tool, so a bash script is enough.
- **Skills:** reusable markdown instructions the agent can load on demand.
- **Providers:** any OpenAI-compatible endpoint, configured in TOML with a base URL, model, and declared capabilities.
- **Sharing:** tools, adapters, skills, and themes are plain files, so they can be shared as repositories or dotfiles.

Possible later addition: an embedded scripting layer (such as Lua) for in-process hooks, only if the external-tool model proves too limiting.

## 10. Architecture overview

Buddy is a small set of cooperating pieces. Most logic lives in Rust; the UI is thin.

- **Omarchy bar widget:** a small native shell widget opens the compact management/chat panel on click. Users create, style, and delete drops there. An optional Waybar adapter remains available.
- **Quickshell UI (QML):** renders the panel, blobs, drag feedback, and chat. It holds no business logic and renders whatever state the daemon sends.
- **Daemon (Rust):** the brain. It contains the agent loop, tool registry, provider adapters, session storage, and Hyprland integration.
- **Control CLI (Rust):** toggle, status, and send commands for Waybar and scripts.
- **IPC:** newline-delimited JSON over a Unix socket between the UI, the CLI, and the daemon.

The daemon subscribes to Hyprland's event socket so anchors stay accurate as windows move, close, or change workspace. On drop, the UI sends only the cursor position; the daemon does all hit-testing and context gathering.

The agent core is deliberately separated from desktop concerns (a provider crate, an agent crate, and an anchor crate), so the engine is reusable and testable without a desktop.

## 11. Model support

- **Target API:** OpenAI-compatible Chat Completions, which covers Ollama, llama.cpp's server, vLLM, LM Studio, and hosted endpoints.
- **Streaming:** responses and tool calls stream to the UI in real time.
- **Declared capabilities:** vision and tool support are set in config, because endpoints cannot reliably report them. Without vision, Buddy falls back to text-only context.
- **Resilience:** malformed tool calls from small local models are returned to the model as errors so it can retry, instead of failing the turn.
- **Reasoning models:** reasoning output can be shown, collapsed, or hidden.

## 12. Sessions, memory, and privacy

- Each blob has an append-only session log stored locally as JSONL, which is easy to inspect, back up, and delete.
- Nothing leaves the machine except requests to the endpoint the user configured. With a local model, nothing leaves at all.
- Screenshots and context are sent only when enabled, and the exact bundle sent can be inspected.
- Users can clear, export, or fork any session.
- No telemetry by default.

## 13. Safety principles

- The agent acts only when asked, at least in the first version. No unprompted actions.
- Approvals show the exact command or change, not a vague summary.
- Capabilities are scoped to the anchor: a blob on a browser does not automatically get shell access.
- All permissions are configurable and visible in plain config files.
- A single global kill switch stops all running agents immediately.

## 14. Example scenarios

1. **Debugging in a terminal.** Drop a blob on a terminal showing a failing build. Ask "why is this failing?" The blob reads the terminal's directory and recent output, inspects the relevant files, explains the cause, and proposes a fix for your approval.
2. **Workspace tidy-up.** Drop a blob on an empty workspace and say "what's open here, and what can I close?" It lists windows with their purposes and offers to close the idle ones.
3. **Reading a page.** Drop a blob on a browser window with a long article and ask for a summary and key claims, using a screenshot or page text.
4. **A custom helper.** A user writes a 20-line script and an anchor rule so any blob dropped on their notes app can append to a daily log. They share it as a small repository.

## 15. Non-goals (for now)

- Cross-platform support (macOS, Windows, X11-only setups).
- A built-in marketplace or cloud sync.
- Voice input and output.
- Autonomous, background, or proactive behavior.
- Replacing coding agents or terminals; Buddy complements them.
- Supporting every model API; the OpenAI-compatible API comes first.

## 16. Roadmap

**Milestone 1: engine.** A provider adapter for OpenAI-compatible APIs, with streaming and tool calls, tested against recorded fixtures and at least two different servers.

**Milestone 2: agent.** The agent loop, core tools, approvals, cancellation, and JSONL sessions, all running headless.

**Milestone 3: daemon and UI shell.** The Unix socket protocol and a basic Quickshell panel with chat and streaming output.

**Milestone 4: anchoring.** Hyprland integration, hit-testing, context bundles, and blobs that follow their windows.

**Milestone 5: drag and drop, and the dot.** Drag feedback, drop behavior, the Waybar module, and polish.

**Milestone 6: extensibility.** TOML anchor rules, external tools, skills, and theming.

**Later:** screenshot-based vision workflows, more anchor adapters, optional scripting, and community sharing.

## 17. Success criteria

- A new user can install Buddy, point it at a local model, and place their first blob in under five minutes.
- Idle resource use is negligible: low memory, near-zero CPU.
- A user can add a working custom tool in under ten minutes without touching Rust.
- The blob reliably follows its window through moves, workspace changes, and closures.
- Users can always see what context was sent and what the agent did.

## 18. Technical decisions for implementers

These are settled choices, so agents building Buddy should follow them rather than reopen them.

**Stack**

- Daemon, control CLI, and all logic in Rust (tokio, serde, reqwest). The Quickshell (QML) UI is thin: it renders what the daemon sends and reports drag events.
- Cargo workspace with these crates: `buddy-proto` (IPC types), `buddy-ai` (message format and provider adapters), `buddy-agent` (loop, tools, sessions), `buddy-anchor` (Hyprland, hit-testing, context), `buddyd` (daemon binary), `buddyctl` (CLI).
- Pi's internals are the reference for the agent loop, tool interface, context files, and JSONL sessions. Port the ideas and verify details against the current pi repository. Pi is MIT-licensed, as is Buddy.

**Model provider**

- Target `/v1/chat/completions` (OpenAI-compatible), not the Responses API, since local servers support it far better.
- Capabilities are declared in TOML, not detected:

```toml
[provider.local]
kind = "custom"
base_url = "http://localhost:11434/v1"
api_key_env = ""
model = "qwen2.5-vl:7b"
vision = true
tools = true
max_tokens_param = "max_tokens"
```

- Streaming is SSE. Tool-call deltas arrive in fragments keyed by `index`, with the id and name in the first fragment and partial JSON in `arguments`. Accumulate per index and parse only when the stream ends.
- Do not trust `finish_reason`: decide whether tools were called from the accumulated calls. Generate an id when a server omits one.
- Request `stream_options.include_usage` but tolerate servers that ignore it. Handle `reasoning_content` as a separate thinking stream.
- Invalid tool arguments go back to the model as an error result, never a crash. Tool results are text-only on most servers, so an image result is sent as a follow-up user message.
- Downscale screenshots before sending them.

**Desktop integration**

- Hit-testing on drop: `hyprctl cursorpos`, then `hyprctl clients -j` and `hyprctl workspaces -j`. The UI sends only coordinates; the daemon decides what is underneath.
- Anchors store a window address or workspace ID, never screen coordinates. The daemon follows Hyprland's socket2 events (`movewindow`, `closewindow`, `workspace`) to move, hide, or retire blobs.
- Screenshots shell out to `grim`.
- IPC is newline-delimited JSON over a Unix socket. Client messages: toggle, drop, send, confirm, cancel. Daemon messages: anchor state, agent events, blob moved.

**Testing**

- Keep request building and stream parsing as pure functions, separate from HTTP code.
- Record real SSE streams from Ollama, llama-server, and a hosted endpoint as fixtures, and replay them in tests. Each new server quirk becomes a new fixture.
- A small mock server that replays fixtures lets the agent loop be tested end to end without a GPU.
- Milestone 1 is done when a streaming reply and a dummy `echo` tool call both work against two different servers.

## 19. Open questions

- **Name and identity:** decided: The name is "Drops".
- **Multiple blobs:** decided: a maximum of 3 blobs at a time. Idle blobs shrink to a small dot, and the one in use expands.
- **Multi-monitor and fractional scaling:** how drag and hit-testing behave across them. To be tested on a real multi-monitor setup once a working build exists.
- **Context depth:** the default amount each adapter reads has to be tuned through use, so it should be adjustable per anchor type.
- **Tool protocol:** decided: a custom stdio protocol.
- **Fork vs. reference:** decided: copy pi's internals almost wholesale.
- **Licensing and distribution:** decided: distribute with an install script. The license is MIT.

## UX decisions (updated)

- The compact panel opens only after clicking the bar widget or a placed drop.
- The widget panel provides create/delete controls and distinct Drop, Cat, and Sprout appearances.
- Model settings accept a pasted endpoint URL and model name; API-key environment variables are optional.
- Dragging displays a smoothly interpolated avatar with pickup and settle animations.
- Closing an attached window deletes its drop; workspace drops are removed manually.
- Reasoning starts collapsed to keep the small panel readable; it expands on click and can be hidden or expanded by default.
