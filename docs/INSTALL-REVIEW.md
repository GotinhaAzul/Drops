> Parent resolution: the engine and external protocol are now implemented, the real release installer has been run in `.test-data/install-real`, and all Rust checks and headless/live desktop integration checks pass. The review below records the earlier implementation-stage findings. Socket fallback is now refused without an explicit private path, and the default file root is a private directory per drop.

# Install & examples review (test sub-agent)

Date: 2026-10-23. Scope: `install.sh`, `examples/*`, this file only.
Build binaries do NOT yet compile (buddy-agent in progress), so the real
`cargo build --release --locked` path could not run.

## Build status (blocker, not owned)

`mise exec -- cargo check --workspace` fails:

```
error[E0583]: file not found for module `tools`
  --> crates/buddy-agent/src/lib.rs:22:1
```

`lib.rs` declares `mod tools;` and `mod provider;`; neither file exists.
No `buddyd`/`buddyctl` binaries, so `tests/smoke.py` and a real
`./install.sh` cannot run yet. Engine owner to fix.

## install.sh: tested with stubbed toolchain

- `bash -n install.sh` → OK. `shellcheck` not installed here.
- Ran sandboxed: stub `cargo`/`mise` on PATH (stub writes dummy
  `target/release/{buddyd,buddyctl}`), with
  `PREFIX=$PWD/.test-data/install-test/prefix` and
  `XDG_CONFIG_HOME=$PWD/.test-data/install-test/config`. Real `$HOME`
  untouched; stubs removed afterwards.
- Verified: `bin/{buddyd,buddyctl,drops-ui}` installed 755; `drops-ui`
  wrapper sets `DROPS_CTL` and execs `quickshell -p .../shell.qml`;
  `share/drops/{ui,examples}` populated; seed config installed mode 600;
  second run preserves a user-edited `config.toml` (no overwrite).
- **Fix applied (owned):** `install -m 644 examples/*` installed the new
  `external-tool.py` non-executable. Added:
  `chmod 755 "$prefix/share/drops/examples/"*.py 2>/dev/null || true`
  Re-ran sandbox install: `external-tool.py` is now `-rwxr-xr-x`, other
  examples stay 644, config still preserved.

## examples/external-tool.py (new, owned)

Tiny `daily_note` tool, stdlib only, executable. Reads one JSON line on
stdin, appends `- <date>: <text>` to `$DAILY_LOG` (default
`<cwd>/daily-log.md`, i.e. the engine's `project_dir`), prints one JSON
object. Accepts `{"name","arguments"}` envelope or bare args;
returns `{"result": ...}` exit 0, `{"error": ...}` exit 1.

Test output (all pass):

```
$ echo '{"name":"daily_note","arguments":{"text":"hello drops"}}' | ./examples/external-tool.py
{"result": "appended to daily-log.md: hello drops"}
$ echo '{"text":"bare form"}' | DAILY_LOG=$T/log.md ./examples/external-tool.py
{"result": "appended to log.md: bare form"}            # exit=0
$ echo '{"arguments":{}}' | ./examples/external-tool.py
{"error": "arguments.text must be a non-empty string"}  # exit=1
$ echo 'not json' | ./examples/external-tool.py
{"error": "request is not valid JSON: ..."}             # exit=1
```

The installed copy under `.test-data/install-test/prefix/...` was
executed the same way and works.

**Open contract item for the engine owner:** the exact stdio field names
are currently specified only by two fragments (`lib.rs`: "writes one JSON
request line to stdin, reads one JSON response from stdout";
`config.toml` comment: "one JSON line in, one JSON object out"). The
referenced `ATTRIBUTION.md` does not exist. This example assumes request
`{name, arguments}` and response `{result}` / `{error}`. `tools.rs` should
confirm these names (and accept at least `result`/`content`/`text` on
output), then document them in `ATTRIBUTION.md`.

## config.toml vs product.md / implementation

- Provider stanza keys match `buddy_ai::ProviderConfig`
  (`base_url/model/api_key_env/vision/tools/max_tokens_param`). Defaults
  differ from the product.md example (`qwen2.5:7b`, `vision=false`,
  `127.0.0.1` vs `qwen2.5-vl:7b`, `vision=true`, `localhost`): the shipped
  default is the safer non-vision one. Fine as-is, flagging only.
- Commented `[[anchor_rules]]` selectors match `AnchorRule` in
  `crates/buddyd/src/rules.rs`
  (`class/title_contains/command_contains/kind/instructions/allow_shell`).
- Commented `[[external_tools]]` keys match the `ExternalTool` struct
  (`name/description/command/risk/parameters`).
- **Appended commented `[appearance]`** per root: keys verified against
  live QML fallbacks in `ui/shell.qml:15-19,61,237`
  (`background #191d19`, `accent #c2e6a0`, `foreground #ecebe4`,
  `muted #92968a`, `blob_size 44`, `roundness 0.43`). Blob-size note
  (20..52 for the ~56px surface) included.

## README vs implementation (flags for root; README not owned)

- Every `buddyctl` command in README exists with the shown syntax
  (`status/drop/send/context/confirm [--deny]/cancel/export/clear/fork/remove/watch`).
  README omits `toggle`, `recall`, `bridge` — consider documenting.
- Waybar example is accurate: `buddyctl status --waybar` exists; `on-click`
  `buddyctl toggle` exists; CSS classes
  (`thinking/waiting/offline`) match the classes `buddyctl` emits.
- Socket default: README says `$XDG_RUNTIME_DIR/drops.sock`; `socket_path()`
  falls back to `$TMPDIR/drops-<user>/drops.sock` when `XDG_RUNTIME_DIR`
  is unset. Minor drift.
- Manual dev-run and installer agree (config preserved, no Waybar/Hyprland/
  systemd/autostart changes). 3-blob cap is enforced (`smoke.py` asserts the
  4th `drop` fails) and matches "up to three drops".
