#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
prefix="${PREFIX:-$HOME/.local}"
if command -v mise >/dev/null 2>&1; then
  mise exec -- cargo build --release --locked
else
  cargo build --release --locked
fi
install -d "$prefix/bin" "$prefix/share/drops/ui" "$prefix/share/drops/examples"
install -m 755 target/release/buddyd target/release/buddyctl "$prefix/bin/"
install -m 755 scripts/drops "$prefix/bin/drops"
install -m 644 ui/*.qml "$prefix/share/drops/ui/"
install -m 644 examples/* "$prefix/share/drops/examples/"
chmod 755 "$prefix/share/drops/examples/"*.py 2>/dev/null || true
cat > "$prefix/bin/drops-ui" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
prefix="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export DROPS_CTL="$prefix/bin/buddyctl"
exec quickshell -p "$prefix/share/drops/ui/shell.qml" "$@"
SCRIPT
chmod 755 "$prefix/bin/drops-ui"
config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/drops"
install -d "$config_dir"
if [[ ! -e "$config_dir/config.toml" ]]; then install -m 600 examples/config.toml "$config_dir/config.toml"; fi
printf 'Installed Drops under %s. Configure %s/config.toml, then run drops.\n' "$prefix" "$config_dir"
