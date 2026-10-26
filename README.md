# Drops

A small desktop companion for Hyprland/Omarchy. Place up to three drops on windows or workspaces, then chat with an OpenAI-compatible model using the current anchor as context. Rust runs the provider, agent, tools, storage and compositor integration; Quickshell renders the interface.

## Run

Requires Linux, a Rust toolchain, Hyprland, and Quickshell with Qt Quick Controls. No model is bundled or started automatically.

```sh
./install.sh --omarchy-widget
```

This installs Drops under `~/.local` and enables a small native widget in the Omarchy top bar. Click the drop icon to open the panel. It leads with a live status header (connection failures appear inline), drop pills when more than one Drop exists, and a selected-Drop card with the chat action. File access, in-window evidence, the six-face appearance row and compact Lively/Sudo switches follow; New drop and delete sit in the footer. The panel stays closed on startup.

- **+** creates a drop (maximum three); **−** deletes the selected drop and conversation, stopping any running turn first.
- **Automatic** folder detection is the default. It refreshes on placement, when reopening a Drop, and before each message. **File access → Change** sets a working directory for the selected Drop, with a folder browser or an absolute path. **Automatic** restores detection from the anchored app. The panel shows the file directory and makes private-folder fallback visible. A selected folder persists across restarts and anchor moves.
- Pick **Drop**, **Cat**, **Sprout**, **Bunny**, **Robot**, or **Cloud** from the appearance row to change that drop’s look. The selected face carries the accent ring.
- **Lively Drops** in the panel enables gentle swaying, a little hop when a reply arrives, expressive faces and extra status badges: **≡** reading, **■** stopping, **×** error, and **✓** unread reply. Thinking keeps its animated dots; approval keeps its **!** cue. Turn it off for the quieter appearance. It starts off and is saved across restarts; avatar choices are always available.
- Drag the avatar onto a window or empty workspace. A floating avatar follows the pointer with pickup and settle animations, and the panel closes after placement.
- A window drop is deleted when its attached window closes. A workspace drop stays until you delete it.
- **⚙** in the panel header opens **Model settings**. A segmented provider control switches between **Custom endpoint** and **Opencode Go**. **Custom endpoint** (the default) keeps the original bring-your-own OpenAI-compatible setup: paste the endpoint URL, enter the model name and, optionally, the name of an environment variable that holds the key. **Opencode Go** asks for an Opencode Go API key, loads the gateway's model list, and lets you pick a model. The capture options and a pinned **Save connection** action stay in reach while the settings body scrolls. Each drop sends its own stable session id, as the gateway recommends for routing and prompt caching. A missing config is supported for first-run setup.
- **Sudo mode** runs mutating and dangerous tools without a per-call approval prompt. Validation, workspace confinement, diff previews and backups still apply, and shell access still depends on the anchored window. It starts off, is saved across restarts, and its switch turns amber while enabled.

Click a placed drop to open its in-place chat bubble. It greets the anchored window, offers contextual quick actions, and grows into a scrollable conversation. Sending collapses the bubble; the reply returns as a short, truncated bubble above the drop, and clicking it or the drop reopens the full conversation. Idle drops shrink to colored dots. Click the anchor label to inspect context; Enter sends, Escape closes, and the stop icon cancels the current turn. The context inspector closes with its **×** button or Escape, and hiding the panel closes it too. `buddyctl cancel` stops all turns.

Reasoning from thinking models streams into one compact, scrollable block instead of the chat itself: it starts collapsed with a word count, expands on click, and can be hidden or shown expanded by default with `[appearance].thinking_mode` (`"collapsed"`, `"hidden"`, or `"shown"`).

`./install.sh` without the widget flag installs the standalone launcher only; run `drops` to toggle its panel. `PREFIX` overrides the install prefix. Existing Drops configuration is preserved; widget updates are backed up outside the plugin discovery directory. The optional Waybar example remains available. The installer makes no Hyprland, autostart, or systemd changes.

For development, run `mise exec -- cargo build --workspace`, then `target/debug/buddyd` and `DROPS_CTL="$PWD/target/debug/buddyctl" quickshell -p ui/shell.qml`. Configuration files are optional; advanced settings such as `project_dir`, capabilities and external tools are illustrated in `examples/config.toml`.

## Control

```sh
buddyctl status
buddyctl toggle
buddyctl create --appearance cat
buddyctl style BLOB_ID sprout
buddyctl sudo on                # off restores per-call approval
buddyctl lively on              # off restores quiet mode
buddyctl configure http://localhost:11434/v1 MODEL_NAME
OPENCODE_API_KEY=... buddyctl configure https://opencode.ai/zen/go/v1 MODEL --mode opencode-go
OPENCODE_API_KEY=... buddyctl models # list models; a key saved in settings also works
buddyctl drop 300 200
buddyctl send BLOB_ID 'What is open here?'
buddyctl context BLOB_ID
buddyctl directory BLOB_ID /absolute/path/to/folder
buddyctl directory BLOB_ID        # restore automatic detection
buddyctl confirm BLOB_ID          # --deny rejects
buddyctl cancel                  # global kill switch
buddyctl export BLOB_ID > session.json
buddyctl clear BLOB_ID
buddyctl fork BLOB_ID
buddyctl remove BLOB_ID
buddyctl watch
```

`DROPS_SOCKET` overrides the socket path (default `$XDG_RUNTIME_DIR/drops.sock`). If `XDG_RUNTIME_DIR` is absent, an explicit private `DROPS_SOCKET` path is required. `buddyd --config PATH --data-dir PATH --headless` supports isolated development without desktop access. Headless anchors are explicitly synthetic workspaces and have no shell capability.

The socket uses newline-delimited JSON and mode 0600. `buddyctl bridge` connects stdin/stdout bidirectionally for the QML shell. `docs/contracts.md` describes crate boundaries. Sessions and blob metadata live under `$XDG_DATA_HOME/drops` (default `~/.local/share/drops`). A custom endpoint reads its key from the environment variable named by `api_key_env`. Opencode Go accepts `OPENCODE_API_KEY` too; a key entered in settings is stored only in the Drops config file, which is written with mode 0600 and is never included in UI state or logs. The settings screen masks it, and leaving the field blank keeps the saved key.

## Working with the anchored app

Each message refreshes `working_context`: current directory, repository root/name, common project manifest names, detection source and any ambiguity. `tool_workspace` records the actual file-tool boundary. A compact `brief` condenses the current anchor/window, workspace and local-document evidence, with explicit freshness, ambiguity and missing-content cues and a likely verification hint from the detected manifest. It is built only from evidence already gathered for that turn. The context inspector and `buddyctl context BLOB_ID` show the brief. Older context snapshots stay in the exported JSONL but are not replayed to the model when a fresh bundle exists, so a previous window or directory cannot dominate a follow-up request.

- **Terminals:** Alacritty, Kitty, Foot, Ghostty, WezTerm, GNOME Terminal/Console/Ptyxis, Konsole, Tilix, Xfce Terminal, XTerm and other recognized terminals are supported. Drops inspects only the anchored process and its bounded descendant tree, preferring foreground processes with a terminal and falling back to a unique shell directory. A shell `cd` is picked up on the next message. Multiple different tab/pane directories remain ambiguous.
- **tmux:** when exactly one client belongs to that window, Drops asks the default tmux server for that client's active pane directory. Custom tmux sockets and ambiguous client matches currently fall back without guessing.
- **File explorers:** Nautilus, Dolphin, Thunar, Nemo, Caja, PCManFM and other recognized explorers use visible location controls tied to that exact window through accessibility, then live `FileManager1.OpenLocations` data from the anchored process when available. Titles disambiguate verified live locations; explicit full paths in a current title can also resolve a folder. Explorer startup arguments and process working directories are never substituted for the current folder after navigation. The tool boundary is the displayed folder, including inside Git repositories. Hidden tabs, conflicting panes, duplicate folder names and remote/virtual locations cannot silently select another folder. Support depends on what each app exposes; the recognized class alone does not guarantee an API or location control is available.
- **Editors and other local apps:** Zed, VS Code/VSCodium, Cursor, Windsurf, additional JetBrains IDEs, Kate, Geany and Sublime Text can expose a workspace through launch arguments. Local file URIs, inline folder flags and JSON `.code-workspace` folder lists are supported. Window-specific accessibility document URI metadata also supports recognized local viewers such as Papers, Evince, Okular, Zathura and image viewers when exposed. Existing absolute paths in current explorer/editor titles can help with reused GUI processes. Browser/page titles and document display names never select a local tool root. Editor files inside one Git repository resolve to that repository. Bare project names never trigger a home-directory search. The opened file from launch arguments is not proof of the current editor tab; ambiguous shared-PID launch contexts remain unresolved.

For local office applications (LibreOffice, OnlyOffice, Word/Excel processes), Drops also checks existing `.docx`/`.xlsx` launch paths and open handles in the anchored process tree. The window title can disambiguate those verified candidates. It does not search your Documents folder, inspect browser profiles, or infer a local path from a browser document title. A file that the app no longer holds open and did not expose in its arguments may need an explicit path.

`document_read` reads saved Word paragraphs or Excel sheets/cells and returns a SHA-256 fingerprint. `document_edit` requires approval and that fingerprint, then creates a **new copy** at a nonexisting output path inside the workspace. It never overwrites the open document. Word editing supports unique text replacements within body paragraphs, including across formatting runs; paragraphs with breaks, fields or tracked changes are refused. Excel editing supports literal values in existing cells in one sheet; formula/error cells and all cells covered by array-formula ranges are read-only, and formula caches can remain stale until the app recalculates the copy. Styles and other archive parts are retained. These tools require Python 3 with only its standard library; no Office installation or API key is needed. Documents are limited to 32 MiB compressed/64 MiB expanded, with smaller XML-part and output limits. Encrypted, signed, macro-enabled, legacy `.doc`/`.xls`, ODF, and web-hosted documents are unsupported. Unsaved app changes are not included.

Folder detection reads location metadata independently of screenshot/content capture settings; it does not type shortcuts, change focus, inspect the clipboard or run project code. Some apps, remote folders, multiple indistinguishable tabs, and blank desktop space do not expose a unique local directory. The panel explicitly shows **Automatic · no folder detected** and the private-folder fallback in those cases; the manual selector remains available.

The chat header shows the selected workspace or saved document when context is available; click it to inspect the evidence and actual tool boundary. Detection refreshes when a message is sent.

In Automatic mode, the root refreshes per message; the Drop is instructed to check it before reusing paths from an earlier turn. A user-selected directory stays fixed until you change it or restore Automatic. Explicit `project_dir` constrains all choices: selected folders must be inside it, and it takes priority over detected application workspaces. Stop an active turn before changing its directory. Unavailable selected directories fail explicitly rather than redirecting edits to another folder. Detection grants no extra shell capability and file mutations still need approval. No editor settings or desktop configuration are changed. A manifest can add a verification hint (for example `cargo test --workspace`), but project code is never run automatically.

Each Drop has small, session-scoped task memory available through `task_memory`: a goal, decisions, pending work and verified results. Memory is bounded, persisted beside the conversation, and injected into later turns. Verified entries must cite conversation or tool evidence, and the model is told to re-check remembered paths when the workspace or anchor changes. Stored paths are confined to the current tool workspace; a workspace change removes path-scoped items and an anchor change removes explicitly anchor-scoped items. Memory is never shared between independent Drops. `clear` and `remove` delete the session and its memory, `fork` deliberately copies both, `export` includes the memory records, and a restart reloads them from the session file.

Drops can explore projects without shell access:

- `workspace_overview` returns directory entries and short, source-labelled previews of README, instruction and common manifest files. Pass `path` to inspect a subproject. It never runs project code; previews are saved file data and may be incomplete.
- `list_files` pages through one directory. `search_files` searches literal text recursively, respects Git ignore rules, and skips common dependency/build directories by default. Use `file_pattern` (for example `**/*.rs`), `case_sensitive=false` for ASCII case-insensitive matching, or explicit `include_hidden` / `include_ignored` controls. Search never traverses symlinks or `.git` directories.
- Both discovery tools return `next_offset` when another result page is available. Pagination rescans current files. `scan_limited=true` means the entry/byte budget was exhausted: narrow `path` to inspect the remaining area. Skipped entries, warnings and coverage notes prevent incomplete searches from being mistaken for proof of absence.
- Search excerpts center on the match, including on long lines, and carry 1-based line and excerpt-column numbers. Structured discovery results stay valid JSON under the output cap.

Basic tool argument checks (required fields, declared types, enums and scalar/array bounds) happen before approval, so common model mistakes can be corrected without prompting you. Tool implementations also enforce their own semantic checks. Text reads reject devices, binary data and invalid UTF-8; actual reads of files, instructions and skills are byte-bounded even if a file grows during reading.

File approvals show a concrete unified diff for `write`, `edit` and `append` before you allow them. A call that cannot apply (missing match, stale `expected_content`, binary data) is reported without an approval prompt. If a file changed after the model read it, the mutation is refused. Every approval also freezes the previewed target: a file appearing, disappearing, changing, or resolving elsewhere while approval is pending is refused, even without a prior read. Large diffs are explicitly truncated; inspect omitted changes before approving. Overwrites and appends are replaced atomically through a temporary file in the same directory; existing permission bits are preserved. The previous content is copied to `.drops/backups` inside the workspace first, with the most recent ten backups kept per file. Paths and symlinks remain confined to the tool workspace, and a denied change performs no write and creates no backup. `write`/`edit`/`append` operate on UTF-8 text and refuse inputs or results larger than 8 MiB; cross-turn stale detection relies on `expected_content`, while within-run reads are tracked automatically.

## Understanding the app under a Drop

Drops reads explicit app context through the matched window’s accessibility interfaces, alongside the existing window-content capture:

- **Browsers:** the displayed document’s URL and title when the browser exposes them. Nested frames are distinguished from the top-level page; equally placed documents stay ambiguous. URLs omit credentials, queries and fragments, so the displayed link may not reproduce the exact page state. No browser profile, history, remote-debugging endpoint or DOM is read.
- **Highlighted text:** actual text-selection ranges in a browser, editor, terminal or document, rather than guesses from nearby text. The selection has a separate 6,000-character budget and partial selections are labelled. Password fields and hidden accessibility subtrees are excluded.
- **File explorers:** selected items from the app’s selection interface. Explicit local URIs or exact file labels in a file-content table/grid are checked against the detected folder. Only existing paths inside both that folder and the Drop’s file boundary are supplied as usable paths. Missing, remote, sidebar-only and escaping symlink paths remain unverified or inaccessible; selecting an item never expands permissions.
- **Editors and document viewers:** an exposed current document URI can identify the actual document, separately from the file passed at launch. Saved files can be read or edited with the existing tools where supported; unsaved highlighted text is available as captured text, not as a claim about disk contents.

Opening a placed Drop refreshes this metadata without reading the whole document, taking a screenshot or contacting the model. Sending gathers it again with the window content. The chat shows the detected page/document and selection, with **Explain selection**, **Rewrite selection**, or explorer-specific quick actions. Rewrites are drafts in chat unless you request an available editing tool. Click the context card to inspect readable details; raw JSON is under **Show technical details**. The refresh button updates the app context while the Drop is idle.

These integrations respect window-content/accessibility capture settings and known lock/visibility restrictions. Availability depends on the app’s accessibility support; missing selections, paths and documents are never invented. The agent can use existing file/document tools within their boundaries, but cannot arbitrarily click or type into an app, or move explorer files without an appropriate available tool. Page contents, selections and filenames are untrusted evidence, never instructions.

## Safety and scope

File tools are confined to one root: the user-selected directory, configured `project_dir`, or uniquely detected application directory, in that order. A selection must stay inside any configured `project_dir`. Without any of those, each Drop uses an empty private directory under the app data directory. User selections reject the filesystem root, home and system directories; symlinks cannot escape a configured boundary. File changes and mutating/dangerous external tools request approval; shell requires a terminal anchor and explicit approval. Window context and model output are untrusted. A local endpoint keeps model requests local; selecting a hosted endpoint sends the displayed context to it. On each message, window drops read matching accessibility text or terminal tmux scrollback and can capture the visible window for OCR. The drop avatar stays visible while its window is read; the floating panel and chat surfaces still hide so screenshots stay clean. The request carries a shortened preview; the read-only `window_content` tool lets the agent search or read a larger on-demand capture of the same window (also accessibility/tmux text, never a browser DOM/HTML dump). Enable “Model supports images” in the endpoint settings to attach pixels to a vision model. The context inspector shows the captured text, image, sources and any missing-content warnings. Capture can be disabled in settings; locked, hidden or occluded windows are not screenshotted. See [window capture](docs/capture/README.md).

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
python3 tests/office-array-formulas.py
# Optional compatibility checks using independently installed Office readers
uv run --with python-docx --with openpyxl python tests/office-compat.py
python3 tests/widget-smoke.py
python3 -m unittest discover -s tests/text_capture -p test_workspace_probe.py -v
PYTHONPATH=tests/text_capture python3 -m unittest test_app_context
python3 tests/qml-interactions.py
python3 tests/qml-context-inspector.py
python3 tests/sudo-smoke.py
python3 tests/qml-opencode-settings.py
python3 tests/lively-smoke.py
python3 tests/qml-bubble.py
DROPS_TEST_LIVELY=1 python3 tests/qml-bubble.py
python3 tests/qml-bubble-position.py
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
