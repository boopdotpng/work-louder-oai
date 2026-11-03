#!/bin/sh
# Install or reinstall the codex-microd per-user launchd agent on macOS.
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

daemon=$(command -v codex-microd) || {
  echo "codex-microd is not on PATH; install the package first (see README.md)" >&2
  exit 1
}

mkdir -p "$agents" "$HOME/Library/Logs"
sed -e "s|@CODEX_MICROD@|$daemon|g" -e "s|@HOME@|$HOME|g" \
  "$here/$label.plist" >"$plist"
plutil -lint "$plist" >/dev/null

launchctl bootout "$domain/$label" 2>/dev/null || true
launchctl bootstrap "$domain" "$plist"

echo "installed $plist"
echo "daemon:  $daemon"
echo "logs:    $HOME/Library/Logs/codex-microd.log"
echo
echo "If the log reports that macOS denied access, enable Input Monitoring for"
echo "the Python interpreter that runs codex-microd:"
echo "  $(head -n 1 "$daemon" | sed 's/^#!//')"
echo "then run: launchctl kickstart -k $domain/$label"
