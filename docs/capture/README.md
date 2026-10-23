# Window content

Drop onto a window, then ask about it. Each message refreshes the attached content:

- Terminals: matching tmux scrollback where available; otherwise visible text through accessibility or screenshot OCR. For example: “What command did I run wrong?”
- Editors: exposed accessibility text and the visible screenshot. For repository-wide inspection, also configure `project_dir`; a window attachment does not grant access to every project file.
- Browsers: exposed page text, with visible screenshot/OCR as fallback. For example: “Summarize this page.” Apps must expose accessibility text for content beyond the visible crop.

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
```

Opt-in live validation: `python3 tests/capture-live.py` creates its own disposable windows and uses a local mock endpoint. It never contacts a configured model service.
