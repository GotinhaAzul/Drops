# Drops

A small desktop companion for Hyprland/Omarchy. Place up to three drops on windows or workspaces, then chat with an OpenAI-compatible model using the current anchor as context. Rust runs the provider, agent, tools, storage and compositor integration; Quickshell renders the interface.

## Run

Requires Linux, a Rust toolchain, Hyprland, Quickshell with Qt Quick Controls, and a configured OpenAI-compatible endpoint. No model is bundled or started automatically.

```sh
mise install
mise exec -- cargo build --workspace
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/drops"
cp -n examples/config.toml "${XDG_CONFIG_HOME:-$HOME/.config}/drops/config.toml"
# Edit config.toml: endpoint, model, and a project_dir for file tools.
target/debug/buddyd
# In a second terminal on your Hyprland desktop:
DROPS_CTL="$PWD/target/debug/buddyctl" quickshell -p ui/shell.qml
```

Or run `./install.sh` to build and install into `~/.local` (`PREFIX` overrides it), then run `drops`. It starts the daemon and UI on demand; subsequent calls toggle the small panel. The installer preserves an existing Drops config and makes no changes to Waybar, Hyprland, autostart, or systemd. Add the example Waybar module manually if wanted; clicking its dot runs `drops toggle`, so the first click also starts Drops.

Click the Waybar dot to open a small drop and chat field. Drag the drop onto a window or empty workspace; the panel closes on placement. Click a placed drop to open its compact conversation. Idle drops shrink to dots. Tiny selectors appear when several drops are placed. History and approvals appear only during a conversation; click the anchor label to inspect its context. Enter sends, Escape closes, and the stop icon cancels the current turn. `buddyctl cancel` is the global kill switch.

## Control

```sh
buddyctl status
buddyctl toggle
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

## Safety and scope

File tools are confined to the configured `project_dir`; when omitted, each drop gets an empty private directory under the app data directory. The filesystem root is refused. File changes and mutating/dangerous external tools request approval; shell requires a terminal anchor and explicit approval. Window context and model output are untrusted. A local endpoint keeps model requests local; selecting a hosted endpoint sends the displayed context to it. Screenshots are a later roadmap feature and are not captured automatically.

The current build is an initial implementation, not a claim that every success criterion is measured. See `docs/STATUS.md` for verified behavior and remaining desktop validation. Provider fixtures and a mock endpoint exercise the engine without a GPU. No live endpoint, model weights, credentials or desktop configuration are provisioned by tests.

## Development

```sh
mise exec -- cargo test --workspace
mise exec -- cargo clippy --workspace --all-targets -- -D warnings
mise exec -- cargo fmt --all -- --check
python3 tests/smoke.py
python3 tests/failure-smoke.py
python3 tests/shutdown-smoke.py
python3 tests/qml-load.py
# Opt-in: creates, moves and closes its own disposable terminal on Hyprland
python3 tests/desktop-smoke.py
# After an isolated install into .test-data/install-real/prefix:
python3 tests/launcher-smoke.py
```

MIT. The agent architecture is adapted from [Earendil Pi](https://github.com/earendil-works/pi); provenance and attribution are in `docs/PI-REFERENCE.md` and `references/`.
