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
die() { printf 'omessage uninstall: %s\n' "$*" >&2; exit 1; }

# PIDs of this user's `omessaged daemon` processes, wherever the binary is
# (including one whose file was already deleted).
daemon_pids() {
  local pid argv
  for pid in $(pgrep -u "$(id -u)" -x omessaged 2>/dev/null || true); do
    mapfile -d '' -t argv <"/proc/$pid/cmdline" 2>/dev/null || continue
    [[ ${argv[1]:-} == daemon ]] && printf '%s\n' "$pid"
  done
  return 0
}

service_active() { systemctl --user is-active --quiet omessaged.service 2>/dev/null; }

# Stop the service and make sure no daemon is left holding the key or relay
# connections. Nothing is removed unless this succeeds.
if systemctl --user cat omessaged.service >/dev/null 2>&1; then
  systemctl --user disable --now omessaged.service >/dev/null 2>&1 ||
    say "systemctl could not stop omessaged cleanly; checking for a running helper"
fi
if service_active; then
  systemctl --user kill --signal=SIGKILL omessaged.service >/dev/null 2>&1 || true
  sleep 1
fi
pids=$(daemon_pids)
if [[ -n $pids ]]; then
  # shellcheck disable=SC2086 # one PID per word
  kill $pids 2>/dev/null || true
  for _ in $(seq 1 10); do
    [[ -z $(daemon_pids) ]] && break
    sleep 0.5
  done
  pids=$(daemon_pids)
  if [[ -n $pids ]]; then
    # shellcheck disable=SC2086
    kill -KILL $pids 2>/dev/null || true
    sleep 0.5
  fi
fi
if service_active || [[ -n $(daemon_pids) ]]; then
  die "omessaged is still running (PIDs: $(daemon_pids | tr '\n' ' ')). Nothing was removed. Stop it with 'systemctl --user stop omessaged' and run this again."
fi

rm -f "$UNIT"
systemctl --user daemon-reload 2>/dev/null || true
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
