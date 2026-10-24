# Window content

Drop onto a window, then ask about it. Each message refreshes the attached content:

- Terminals: matching tmux scrollback where available; otherwise visible text through accessibility or screenshot OCR. For example: “What command did I run wrong?”
- Editors: exposed accessibility text and the visible screenshot. Workspace detection can use existing paths from editor launch arguments to identify the repository. When that evidence is unavailable or ambiguous, configure `project_dir` for repository-wide file tools. The current editor tab is not inferred from launch arguments.
- Browsers: exposed page text, with visible screenshot/OCR as fallback. For example: “Summarize this page.” Apps must expose accessibility text for content beyond the visible crop.

## Fuller content on demand

The request context contains a shortened preview (24 000 characters by default). When **`full_text`** is enabled, a window drop also exposes the read-only `window_content` tool:

- `read` pages through the full captured text by character offset.
- `search` finds ASCII case-insensitive matches and returns their character offsets.
- `info` lists provenance, character counts and truncation warnings.

The larger capture runs only when the agent calls the tool, so ordinary sends keep the small preview and the short timeout. It is cached for the rest of that turn. It reads the same accessibility/terminal sources as the preview; it does not take a screenshot, read a browser profile, scrape the DOM/HTML, or use remote debugging. A browser page therefore yields the text the application exposes through AT-SPI, not its raw markup. The tool is absent for workspace drops, when capture is disabled, or when the provider has tools disabled.

Tool results are sent back to the configured endpoint, so a `read` call can send a larger portion of the window text than the preview. Keep the endpoint local if the window content should not leave the machine.

Open the widget’s endpoint settings. **Read attached window content** enables capture. **Use screenshots / OCR fallback** enables window screenshots. Check **Model supports images** only for a vision-capable model: images become actual image attachments, while text models receive extracted text only. The selected endpoint receives this content with the prompt.

Click the content indicator in chat to inspect text, image, provenance and warnings. Capture occurs at send time, not continuously. If content is missing, the assistant is instructed to disclose that rather than guess from the window title.

The helper matches the window’s process and title, skips protected/password accessibility nodes, and never uses the clipboard, keyboard input, browser profiles or remote debugging. Screenshot crops are limited to the visible attached window. Locked desktops block content capture; hidden, occluded or partly offscreen windows cannot supply screenshots. Text and image sizes, subprocess output, timeouts and cancellation are bounded. OCR may misread characters; terminal scrollback and complete page text depend on app support.

Dependencies: `grim` for screenshots, `tesseract` for OCR, and `/usr/bin/python` with PyGObject/AT-SPI for accessibility. Missing optional helpers produce inspectable warnings. No desktop permission settings are changed.

Advanced config (all optional):

```toml
[capture]
enabled = true
accessibility = true
terminal = true
screenshots = true
ocr = true
max_text_chars = 24000
max_image_edge = 1280
timeout_ms = 2500
# On-demand full content for the window_content tool.
full_text = true
full_max_text_chars = 500000
full_max_nodes = 50000
full_max_depth = 120
full_budget_ms = 8000
full_timeout_ms = 12000
```

Opt-in live validation: `python3 tests/capture-live.py` creates its own disposable windows and uses a local mock endpoint. It never contacts a configured model service.
