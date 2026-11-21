#!/usr/bin/env bash
# Remove the omessaged helper and its user service.
#
# Keeps your identity key and messages in ~/.local/share/omessage unless
# you pass --purge. Run this before `omarchy plugin remove`.
set -euo pipefail

DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
CACHE_HOME="${XDG_CACHE_HOME:-$HOME/.cache}"
UNIT="$CONFIG_HOME/systemd/user/omessaged.service"
PURGE=0
ASSUME_YES=0
for arg in "$@"; do
  case "$arg" in
  --purge) PURGE=1 ;;
  --yes | -y) ASSUME_YES=1 ;;
  -h | --help)
    echo "Usage: uninstall.sh [--purge [--yes]]"
    exit 0
    ;;
  *)
    echo "unknown option: $arg" >&2
    exit 2
    ;;
  esac
done

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

if systemctl --user list-unit-files omessaged.service >/dev/null 2>&1; then
  systemctl --user disable --now omessaged.service >/dev/null 2>&1 || true
fi
rm -f "$UNIT"
systemctl --user daemon-reload || true
say "Stopped and removed the omessaged user service."

rm -rf "$DATA_HOME/omessage/bin" "$CACHE_HOME/omessage"
rm -f "${XDG_RUNTIME_DIR:-/nonexistent}/omessage/omessaged.sock"
rmdir "${XDG_RUNTIME_DIR:-/nonexistent}/omessage" 2>/dev/null || true
say "Removed the helper binary."

if ((PURGE)); then
  if ((!ASSUME_YES)); then
    printf 'Delete your Omessage identity key and all messages? This cannot be undone. [y/N] '
    read -r answer || answer=
    [[ $answer == [yY]* ]] || { say "Kept $DATA_HOME/omessage"; exit 0; }
  fi
  rm -rf "$DATA_HOME/omessage" "$CONFIG_HOME/omessage"
  say "Deleted identity, messages and relay settings."
else
  say "Kept your identity and messages in $DATA_HOME/omessage (use --purge to delete)."
fi
