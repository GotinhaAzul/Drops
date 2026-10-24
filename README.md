# Drops

A small desktop companion for Hyprland/Omarchy. Place up to three drops on windows or workspaces, then chat with an OpenAI-compatible model using the current anchor as context. Rust runs the provider, agent, tools, storage and compositor integration; Quickshell renders the interface.

## Run

Requires Linux, a Rust toolchain, Hyprland, and Quickshell with Qt Quick Controls. No model is bundled or started automatically.

```sh
./install.sh --omarchy-widget
```

This installs Drops under `~/.local` and enables a small native widget in the Omarchy top bar. Click the drop icon to open its compact panel. The panel stays closed on startup.

- **+** creates a drop (maximum three); **−** deletes the selected drop and conversation, stopping any running turn first.
- Choose **Drop**, **Cat**, or **Sprout** to change that drop’s appearance.
- Drag the avatar onto a window or empty workspace. A floating avatar follows the pointer with pickup and settle animations, and the panel closes after placement.
- A window drop is deleted when its attached window closes. A workspace drop stays until you delete it.
- **⚙ Model connection** lets you paste the endpoint URL, enter the model name and save. For authentication, enter the name of an existing API-key environment variable; the key itself is never stored in TOML. A missing config is supported for first-run setup.

Click a placed drop to open its in-place chat bubble. It greets the anchored window, offers contextual quick actions, and grows into a scrollable conversation. Sending collapses the bubble; the reply returns as a short, truncated bubble above the drop, and clicking it or the drop reopens the full conversation. Idle drops shrink to colored dots. Click the anchor label to inspect context; Enter sends, Escape closes, and the stop icon cancels the current turn. `buddyctl cancel` stops all turns.

Reasoning from thinking models streams into one compact, scrollable block instead of the chat itself: it starts collapsed with a word count, expands on click, and can be hidden or shown expanded by default with `[appearance].thinking_mode` (`"collapsed"`, `"hidden"`, or `"shown"`).

`./install.sh` without the widget flag installs the standalone launcher only; run `drops` to toggle its panel. `PREFIX` overrides the install prefix. Existing Drops configuration is preserved; widget updates are backed up outside the plugin discovery directory. The optional Waybar example remains available. The installer makes no Hyprland, autostart, or systemd changes.

For development, run `mise exec -- cargo build --workspace`, then `target/debug/buddyd` and `DROPS_CTL="$PWD/target/debug/buddyctl" quickshell -p ui/shell.qml`. Configuration files are optional; advanced settings such as `project_dir`, capabilities and external tools are illustrated in `examples/config.toml`.

## Control

```sh
buddyctl status
buddyctl toggle
buddyctl create --appearance cat
buddyctl style BLOB_ID sprout
buddyctl configure http://localhost:11434/v1 MODEL_NAME
buddyctl drop 300 200
buddyctl send BLOB_ID 'What is open here?'
buddyctl context BLOB_ID
buddyctl confirm BLOB_ID          # --deny rejects
buddyctl cancel                  # global kill switch
buddyctl export BLOB_ID > session.json
buddyctl clear BLOB_ID
buddyctl fork BLOB_ID
buddyctl remove BLOB_ID
buddyctl watch
```

`DROPS_SOCKET` overrides the socket path (default `$XDG_RUNTIME_DIR/drops.sock`). If `XDG_RUNTIME_DIR` is absent, an explicit private `DROPS_SOCKET` path is required. `buddyd --config PATH --data-dir PATH --headless` supports isolated development without desktop access. Headless anchors are explicitly synthetic workspaces and have no shell capability.

The socket uses newline-delimited JSON and mode 0600. `buddyctl bridge` connects stdin/stdout bidirectionally for the QML shell. `docs/contracts.md` describes crate boundaries. Sessions and blob metadata live under `$XDG_DATA_HOME/drops` (default `~/.local/share/drops`); API keys are read from the environment variable named by `api_key_env` and are never written by Drops.

## Working with the anchored app

Each message refreshes `working_context`: current directory, repository root/name, common project manifest names, detection source and any ambiguity. `tool_workspace` records the actual file-tool boundary. Inspect both through the context inspector or `buddyctl context BLOB_ID`.

- **Terminals:** Drops inspects only the anchored process and its bounded descendant tree, preferring foreground processes with a terminal and falling back to a unique shell directory. A shell `cd` is picked up on the next message. Multiple different tab/pane directories remain ambiguous.
- **tmux:** when exactly one client belongs to that window, Drops asks the default tmux server for that client's active pane directory. Custom tmux sockets and ambiguous client matches currently fall back without guessing.
- **Editors:** Zed, VS Code/VSCodium, Cursor, Windsurf and supported JetBrains classes expose a workspace when their process command line contains an existing directory or opened file path. Files inside one Git repository resolve to that repository. The opened file is launch context, not a claim about the selected editor tab. GUI reuse and multiple windows sharing a PID may not expose enough evidence; no repo is inferred from a title.

For local office applications (LibreOffice, OnlyOffice, Word/Excel processes), Drops also checks existing `.docx`/`.xlsx` launch paths and open handles in the anchored process tree. The window title can disambiguate those verified candidates. It does not search your Documents folder, inspect browser profiles, or infer a local path from a browser document title. A file that the app no longer holds open and did not expose in its arguments may need an explicit path.

`document_read` reads saved Word paragraphs or Excel sheets/cells and returns a SHA-256 fingerprint. `document_edit` requires approval and that fingerprint, then creates a **new copy** at a nonexisting output path inside the workspace. It never overwrites the open document. Word editing supports unique text replacements within body paragraphs, including across formatting runs; paragraphs with breaks, fields or tracked changes are refused. Excel editing supports literal values in existing cells in one sheet; formula/error cells are read-only, and formula caches can remain stale until the app recalculates the copy. Styles and other archive parts are retained. These tools require Python 3 with only its standard library; no Office installation or API key is needed. Documents are limited to 32 MiB compressed/64 MiB expanded, with smaller XML-part and output limits. Encrypted, signed, macro-enabled, legacy `.doc`/`.xls`, ODF, and web-hosted documents are unsupported. Unsaved app changes are not included.

The chat header shows the selected workspace or saved document when context is available; click it to inspect the evidence and actual tool boundary. Detection refreshes when a message is sent.

The selected root refreshes per message; the Drop is instructed to check it before reusing paths from an earlier turn. Explicit `project_dir` always wins, including when the detected application workspace differs. Detection grants no extra shell capability and file mutations still need approval. No editor settings or desktop configuration are changed.

## Safety and scope

File tools are confined to the configured `project_dir`; when omitted, each drop gets an empty private directory under the app data directory. The filesystem root is refused. File changes and mutating/dangerous external tools request approval; shell requires a terminal anchor and explicit approval. Window context and model output are untrusted. A local endpoint keeps model requests local; selecting a hosted endpoint sends the displayed context to it. On each message, window drops read matching accessibility text or terminal tmux scrollback and can capture the visible window for OCR. The drop avatar stays visible while its window is read; the floating panel and chat surfaces still hide so screenshots stay clean. The request carries a shortened preview; the read-only `window_content` tool lets the agent search or read a larger on-demand capture of the same window (also accessibility/tmux text, never a browser DOM/HTML dump). Enable “Model supports images” in the endpoint settings to attach pixels to a vision model. The context inspector shows the captured text, image, sources and any missing-content warnings. Capture can be disabled in settings; locked, hidden or occluded windows are not screenshotted. See [window capture](docs/capture/README.md).

The current build is an initial implementation, not a claim that every success criterion is measured. See `docs/STATUS.md` for verified behavior and remaining desktop validation. Provider fixtures and a mock endpoint exercise the engine without a GPU. No live endpoint, model weights, credentials or desktop configuration are provisioned by tests.

## Development

```sh
mise exec -- cargo test --workspace
mise exec -- cargo clippy --workspace --all-targets -- -D warnings
mise exec -- cargo fmt --all -- --check
# Optional live checks: contact DuckDuckGo and public Rust documentation
mise exec -- cargo test -p buddy-agent live_ -- --ignored
python3 tests/smoke.py
python3 tests/office-tools.py
# Optional compatibility checks using independently installed Office readers
uv run --with python-docx --with openpyxl python tests/office-compat.py
python3 tests/widget-smoke.py
python3 tests/qml-interactions.py
python3 tests/qml-bubble.py
python3 tests/failure-smoke.py
python3 tests/window-content-smoke.py
python3 tests/shutdown-smoke.py
python3 tests/qml-load.py
python3 tests/qml-thinking.py
# Opt-in: creates, moves and closes its own disposable terminal on Hyprland
python3 tests/desktop-smoke.py
# After an isolated install into .test-data/install-real/prefix:
python3 tests/launcher-smoke.py
```

MIT. The agent architecture is adapted from [Earendil Pi](https://github.com/earendil-works/pi); provenance and attribution are in `docs/PI-REFERENCE.md` and `references/`.
