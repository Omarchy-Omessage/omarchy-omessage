#!/usr/bin/env bash
# shellcheck disable=SC2034 # $out is read inside check's eval strings
# Tests for install.sh and uninstall.sh against a fake systemctl, in a
# throwaway HOME. Usage: tests/scripts_test.sh path/to/omessaged
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REAL_BIN="$(realpath "${1:?usage: scripts_test.sh path/to/omessaged}")"
T=$(mktemp -d)
trap 'pkill -u "$(id -u)" -f "$T/" 2>/dev/null; rm -rf "$T"' EXIT
fails=0

ok() { printf 'ok   %s\n' "$1"; }
bad() { printf 'FAIL %s\n' "$1"; fails=$((fails + 1)); }
check() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }

# Fake systemctl: `is-active` follows $T/active (0/1), everything else
# succeeds, `disable --now` fails if $T/stop-fails exists.
mkdir -p "$T/fakebin"
cat >"$T/fakebin/systemctl" <<FAKE
#!/usr/bin/env bash
for a in "\$@"; do
  case "\$a" in
  is-active) [[ \$(cat "$T/active" 2>/dev/null) == 1 ]]; exit ;;
  disable) [[ -e "$T/stop-fails" ]] && exit 1; echo 0 >"$T/active"; exit 0 ;;
  restart) echo 1 >"$T/active"; exit 0 ;;
  esac
done
exit 0
FAKE
chmod +x "$T/fakebin/systemctl"

reset_home() {
  rm -rf "${T:?}/home" "${T:?}/run"
  mkdir -p "$T/home" "$T/run"
  chmod 700 "$T/run"
  echo 0 >"$T/active"
  rm -f "$T/stop-fails"
}
run_env() {
  env -i PATH="$T/fakebin:/usr/bin:/bin" HOME="$T/home" XDG_RUNTIME_DIR="$T/run" "$@"
}

INSTALLED="$T/home/.local/share/omessage/bin/omessaged"
UNIT="$T/home/.config/systemd/user/omessaged.service"
GOOD_SHA=$(sha256sum "$REAL_BIN" | cut -d' ' -f1)

# --- install.sh
reset_home
out=$(run_env OMESSAGE_HELPER_BIN="$REAL_BIN" OMESSAGE_HELPER_SHA256="$(printf '0%.0s' {1..64})" \
  bash "$ROOT/install.sh" 2>&1)
rc=$?
check "install: checksum mismatch fails" "[[ $rc -ne 0 ]]"
check "install: mismatch says so" "[[ \$out == *'checksum mismatch'* ]]"
check "install: mismatch installs nothing" "[[ ! -e $INSTALLED && ! -e $UNIT ]]"

reset_home
cp /bin/true "$T/notomessaged"
out=$(run_env OMESSAGE_HELPER_BIN="$T/notomessaged" OMESSAGE_HELPER_SHA256="$(sha256sum "$T/notomessaged" | cut -d' ' -f1)" \
  bash "$ROOT/install.sh" 2>&1)
check "install: other binary refused" "[[ \$out == *'is not omessaged'* && ! -e $INSTALLED ]]"

reset_home
printf '#!/bin/sh\nexit 1\n' >"$T/broken"
chmod +x "$T/broken"
out=$(run_env OMESSAGE_HELPER_BIN="$T/broken" bash "$ROOT/install.sh" 2>&1)
check "install: binary that doesn't run is refused" "[[ \$out == *'does not run'* && ! -e $INSTALLED ]]"

reset_home
out=$(run_env OMESSAGE_HELPER_BIN="$REAL_BIN" OMESSAGE_HELPER_SHA256="$GOOD_SHA" bash "$ROOT/install.sh" 2>&1)
rc=$?
check "install: good checksum installs" "[[ $rc -eq 0 && -x $INSTALLED && -f $UNIT ]]"
check "install: unit points at the helper" "grep -q 'ExecStart=\"$INSTALLED\" daemon' '$UNIT'"
check "install: fresh unit makes no backup" "! compgen -G '$UNIT.bak-*' >/dev/null"

out=$(run_env OMESSAGE_HELPER_BIN="$REAL_BIN" OMESSAGE_HELPER_SHA256="$GOOD_SHA" bash "$ROOT/install.sh" 2>&1)
check "install: unchanged unit makes no backup" "! compgen -G '$UNIT.bak-*' >/dev/null"

echo "Environment=EDITED=1" >>"$UNIT"
out=$(run_env OMESSAGE_HELPER_BIN="$REAL_BIN" OMESSAGE_HELPER_SHA256="$GOOD_SHA" bash "$ROOT/install.sh" 2>&1)
check "install: edited unit is backed up" "grep -q EDITED $UNIT.bak-* && ! grep -q EDITED '$UNIT'"
check "install: points to drop-ins" "[[ \$out == *'systemctl --user edit omessaged'* ]]"

# --- uninstall.sh
mkdir -p "$T/home/.local/share/omessage"
echo data >"$T/home/.local/share/omessage/marker"

echo 1 >"$T/active"
touch "$T/stop-fails"
out=$(run_env bash "$ROOT/uninstall.sh" --purge --yes 2>&1)
rc=$?
check "uninstall: service that won't stop fails" "[[ $rc -ne 0 && \$out == *'still running'* ]]"
check "uninstall: nothing removed when stop fails" "[[ -x $INSTALLED && -f $UNIT && -f $T/home/.local/share/omessage/marker ]]"

# A real daemon outside systemd must be found and stopped before removal.
rm -f "$T/stop-fails"
echo 0 >"$T/active"
run_env "$INSTALLED" daemon >"$T/daemon.log" 2>&1 &
for _ in $(seq 1 40); do [[ -S $T/run/omessage/omessaged.sock ]] && break; sleep 0.25; done
check "uninstall: test daemon started" "[[ -S $T/run/omessage/omessaged.sock ]]"
run_env bash "$ROOT/uninstall.sh" >"$T/uninstall.log" 2>&1
rc=$?
check "uninstall: stops a stray daemon and succeeds" "[[ $rc -eq 0 ]] && ! pgrep -u $(id -u) -f '$INSTALLED daemon' >/dev/null"
check "uninstall: removes helper and unit" "[[ ! -e $INSTALLED && ! -e $UNIT ]]"
check "uninstall: keeps identity without --purge" "[[ -f $T/home/.local/share/omessage/marker ]]"

run_env bash "$ROOT/uninstall.sh" --purge --yes >/dev/null 2>&1
rc=$?
check "uninstall --purge --yes deletes data" "[[ $rc -eq 0 && ! -e $T/home/.local/share/omessage ]]"

echo
if ((fails)); then
  echo "$fails test(s) failed"
  exit 1
fi
echo "all script tests passed"
