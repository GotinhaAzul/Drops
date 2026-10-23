#!/usr/bin/env bash
set -euo pipefail
widget=false
if [[ "${1:-}" == --omarchy-widget ]]; then widget=true; shift; fi
if (( $# )); then printf 'Usage: ./install.sh [--omarchy-widget]\n' >&2; exit 2; fi
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
install -m 644 ui/*.qml ui/*.js "$prefix/share/drops/ui/"
install -m 644 examples/* "$prefix/share/drops/examples/"
install -d "$prefix/share/drops/omarchy/local.drops"
install -m 644 integrations/omarchy/local.drops/* "$prefix/share/drops/omarchy/local.drops/"
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

if [[ "$widget" == true ]]; then
  plugin_dir="${XDG_CONFIG_HOME:-$HOME/.config}/omarchy/plugins/local.drops"
  if [[ -d "$plugin_dir" ]]; then
    backup_dir="${XDG_STATE_HOME:-$HOME/.local/state}/drops/plugin-backups"
    install -d "$backup_dir"
    cp -a "$plugin_dir" "$backup_dir/local.drops-$(date +%s)"
  fi
  install -d "$plugin_dir"
  install -m 644 integrations/omarchy/local.drops/* "$plugin_dir/"
  omarchy plugin validate "$plugin_dir"
  omarchy-shell shell rescanPlugins
  omarchy plugin enable local.drops --section right
  printf 'Drops widget enabled in the Omarchy top bar. Click it to manage drops and set the model endpoint.\n'
fi
