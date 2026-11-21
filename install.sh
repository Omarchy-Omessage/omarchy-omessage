#!/usr/bin/env bash
# Install (or update) the omessaged helper for the Omessage plugin.
#
# User-level only: writes ~/.local/share/omessage/bin/omessaged and
# ~/.config/systemd/user/omessaged.service, then enables the user service.
# It does not touch any Omarchy configuration. No root access is needed.
#
# By default the helper is built from ./helper with cargo (a recent stable Rust).
# Set OMESSAGE_HELPER_BIN=/path/to/omessaged to install a binary you built
# yourself instead.
set -euo pipefail

PLUGIN_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
CACHE_HOME="${XDG_CACHE_HOME:-$HOME/.cache}"
BIN_DIR="$DATA_HOME/omessage/bin"
TARGET_BIN="$BIN_DIR/omessaged"
UNIT_DIR="$CONFIG_HOME/systemd/user"
UNIT="$UNIT_DIR/omessaged.service"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf 'omessage install: %s\n' "$*" >&2; exit 1; }

command -v systemctl >/dev/null || die "systemd is required"

src_bin="${OMESSAGE_HELPER_BIN:-}"
if [[ -z $src_bin ]]; then
  command -v cargo >/dev/null ||
    die "cargo not found. Install Rust first (Arch package: rust), or set OMESSAGE_HELPER_BIN to a prebuilt omessaged."
  say "Building omessaged from $PLUGIN_DIR/helper (first build takes a few minutes)"
  # Build outside the plugin folder so the shell's plugin watcher never
  # sees build artifacts.
  export CARGO_TARGET_DIR="$CACHE_HOME/omessage/target"
  cargo build --release --locked --manifest-path "$PLUGIN_DIR/helper/Cargo.toml"
  src_bin="$CARGO_TARGET_DIR/release/omessaged"
fi
[[ -x $src_bin ]] || die "helper binary not found or not executable: $src_bin"

say "Installing helper to $TARGET_BIN"
mkdir -p "$BIN_DIR"
install -m 0755 "$src_bin" "$TARGET_BIN.new"
mv -f "$TARGET_BIN.new" "$TARGET_BIN"

say "Installing user service $UNIT"
mkdir -p "$UNIT_DIR"
# Quote the path for systemd (% is a specifier, \ and " are escapes), then
# escape it for the sed replacement.
unit_bin=${TARGET_BIN//\\/\\\\}
unit_bin=${unit_bin//\"/\\\"}
unit_bin=\"${unit_bin//%/%%}\"
sed_bin=$(printf '%s' "$unit_bin" | sed 's/[\\&|]/\\&/g')
sed "s|@OMESSAGED@|$sed_bin|" "$PLUGIN_DIR/systemd/omessaged.service" >"$UNIT.new"
mv -f "$UNIT.new" "$UNIT"

systemctl --user daemon-reload
systemctl --user enable omessaged.service >/dev/null
systemctl --user restart omessaged.service

for _ in $(seq 1 20); do
  npub=$("$TARGET_BIN" id 2>/dev/null || true)
  [[ $npub == npub1* ]] && break
  sleep 0.5
done

if systemctl --user is-active --quiet omessaged.service; then
  say "Omessage helper is running."
  [[ ${npub:-} == npub1* ]] && say "Your Omessage address (npub): $npub"
  say "Open the Omessage icon in your bar to start chatting."
else
  die "the service did not start; see: journalctl --user -u omessaged -n 50"
fi
