#!/bin/sh
# Install or reinstall the codex-micro per-user launchd agent on macOS.
#
#   ./launchd/install.sh              install, using codex-micro from PATH
#   ./launchd/install.sh --uninstall  stop and remove the agent
set -eu

label=com.github.boopdotpng.codex-microd
here=$(cd "$(dirname "$0")" && pwd)
domain="gui/$(id -u)"
agents="$HOME/Library/LaunchAgents"
plist="$agents/$label.plist"

if [ "${1:-}" = "--uninstall" ]; then
  launchctl bootout "$domain/$label" 2>/dev/null || true
  rm -f "$plist"
  echo "removed $label"
  exit 0
fi

binary=$(command -v codex-micro) || {
  echo "codex-micro is not on PATH; run: cargo install --path ." >&2
  exit 1
}

mkdir -p "$agents" "$HOME/Library/Logs"
sed -e "s|@CODEX_MICRO@|$binary|g" -e "s|@HOME@|$HOME|g" \
  "$here/$label.plist" >"$plist"
plutil -lint "$plist" >/dev/null

launchctl bootout "$domain/$label" 2>/dev/null || true
launchctl bootstrap "$domain" "$plist"

echo "installed $plist"
echo "binary: $binary"
echo "logs:   $HOME/Library/Logs/codex-microd.log"
echo
echo "If the log says macOS denied access, enable Input Monitoring for"
echo "$binary in System Settings > Privacy & Security > Input Monitoring,"
echo "then run: launchctl kickstart -k $domain/$label"
