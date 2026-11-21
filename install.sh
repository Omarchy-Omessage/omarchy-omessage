#!/usr/bin/env bash
# Install (or update) the omessaged helper for the Omessage plugin.
#
# User-level only: writes ~/.local/share/omessage/bin/omessaged and
# ~/.config/systemd/user/omessaged.service, then enables the user service.
# It does not touch any Omarchy configuration. No root access is needed.
#
# By default the helper is built from ./helper with cargo (Rust 1.89+).
# To install a prebuilt binary instead, set OMESSAGE_HELPER_BIN=/path/to/omessaged
# and OMESSAGE_HELPER_SHA256=<its sha256>; the install stops if they don't match.
#
# The unit file is regenerated on every install. Put local changes in a
# drop-in (systemctl --user edit omessaged); if the unit file itself was
# edited, it is backed up before being replaced.
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
[[ -f $src_bin ]] || die "helper binary not found: $src_bin"

mkdir -p "$BIN_DIR"
# Check the copy we will install, not the original, so the file can't change
# between the check and the install.
staged="$TARGET_BIN.new"
trap 'rm -f "$staged"' EXIT
install -m 0755 "$src_bin" "$staged"

want_sha="${OMESSAGE_HELPER_SHA256:-}"
if [[ -n $want_sha ]]; then
  want_sha=${want_sha%% *}
  want_sha=${want_sha,,}
  [[ $want_sha =~ ^[0-9a-f]{64}$ ]] || die "OMESSAGE_HELPER_SHA256 is not a sha256 hash"
  have_sha=$(sha256sum "$staged" | cut -d' ' -f1)
  [[ $have_sha == "$want_sha" ]] ||
    die "checksum mismatch for $src_bin (expected $want_sha, got $have_sha); nothing was installed"
  say "Checksum OK ($have_sha)"
elif [[ -n ${OMESSAGE_HELPER_BIN:-} ]]; then
  say "Warning: OMESSAGE_HELPER_SHA256 is not set, so the binary's checksum was not verified."
fi

# Make sure it runs and is really omessaged before replacing anything.
version=$("$staged" --version 2>/dev/null) || die "$src_bin does not run on this system; nothing was installed"
[[ $version == "omessaged "* ]] || die "$src_bin is not omessaged; nothing was installed"
want_version=$(sed -n 's/^ *"version": *"\([^"]*\)".*/\1/p' "$PLUGIN_DIR/manifest.json" | head -n1)
if [[ -n $want_version && $version != "omessaged $want_version" ]]; then
  say "Warning: installing $version, but the plugin is version $want_version."
fi

say "Installing $version to $TARGET_BIN"
mv -f "$staged" "$TARGET_BIN"
trap - EXIT

say "Installing user service $UNIT"
mkdir -p "$UNIT_DIR"
# Quote the path for systemd (% is a specifier, \ and " are escapes), then
# escape it for the sed replacement.
unit_bin=${TARGET_BIN//\\/\\\\}
unit_bin=${unit_bin//\"/\\\"}
unit_bin=\"${unit_bin//%/%%}\"
sed_bin=$(printf '%s' "$unit_bin" | sed 's/[\\&|]/\\&/g')
sed "s|@OMESSAGED@|$sed_bin|" "$PLUGIN_DIR/systemd/omessaged.service" >"$UNIT.new"
if [[ -f $UNIT ]] && ! cmp -s "$UNIT" "$UNIT.new"; then
  backup="$UNIT.bak-$(date +%Y%m%d-%H%M%S)"
  cp -p "$UNIT" "$backup"
  say "Your edited $UNIT was saved to $backup."
  say "Put local changes in a drop-in instead (systemctl --user edit omessaged); drop-ins survive reinstalls."
fi
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
