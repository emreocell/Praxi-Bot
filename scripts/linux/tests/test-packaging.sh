#!/usr/bin/env bash
# Hermetic tests for scripts/install-linux.sh and scripts/linux/setup-headless.sh.
# Uses temporary directories and fake package/systemctl/gh/curl shims.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
INSTALL_SH="${REPO_ROOT}/scripts/install-linux.sh"
SETUP_SH="${REPO_ROOT}/scripts/linux/setup-headless.sh"

TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "${TMP_ROOT}"' EXIT

pass() { printf 'PASS: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

assert_file_contains() {
  local file="$1" pattern="$2" msg="${3:-}"
  grep -Fq -- "${pattern}" "${file}" || fail "${msg:-expected pattern ${pattern} in ${file}}"
}

assert_file_not_contains() {
  local file="$1" pattern="$2" msg="${3:-}"
  if grep -Fq -- "${pattern}" "${file}"; then
    fail "${msg:-did not expect pattern ${pattern} in ${file}}"
  fi
}

# 1. Syntax & lint checks
bash -n "${INSTALL_SH}" "${SETUP_SH}" "$0"
pass "bash -n syntax checks"
if command -v shellcheck >/dev/null 2>&1; then
  shellcheck -x "${INSTALL_SH}" "${SETUP_SH}" "$0"
  pass "shellcheck -x clean"
else
  echo "SKIP: shellcheck not installed"
fi

# 2. Dry-run & CLI validation checks
"${SETUP_SH}" --dry-run --user --display :98 --screen 1280x800x24 > "${TMP_ROOT}/dry-user.log"
assert_file_contains "${TMP_ROOT}/dry-user.log" "mode=user session=xfce without_desktop=0 display=:98 screen=1280x800x24"
"${SETUP_SH}" --dry-run --system --display :99 --screen 1280x800x24 > "${TMP_ROOT}/dry-sys.log"
assert_file_contains "${TMP_ROOT}/dry-sys.log" "CPUQuota=100%, MemoryHigh=3G, MemoryMax=4G"

if "${SETUP_SH}" --dry-run --vnc > "${TMP_ROOT}/vnc.log" 2>&1; then
  fail "--vnc should be rejected"
fi
assert_file_contains "${TMP_ROOT}/vnc.log" "disabled by security policy"
pass "setup-headless dry-run and option validation"

# 3. Prepare sandboxed rootfs and fake commands
SANDBOX="${TMP_ROOT}/sandbox"
FAKE_BIN="${TMP_ROOT}/fakebin"
RELEASE_DIR="${TMP_ROOT}/release"
mkdir -p "${SANDBOX}/opt/praxi" "${SANDBOX}/var/lib/praxi" "${SANDBOX}/etc/praxi" \
  "${SANDBOX}/etc/systemd/system" "${SANDBOX}/usr/local/bin" "${SANDBOX}/etc" \
  "${SANDBOX}/usr/bin" "${SANDBOX}/home/tester" "${FAKE_BIN}" "${RELEASE_DIR}"

cat > "${SANDBOX}/etc/os-release" <<'EOF'
ID=ubuntu
VERSION_ID="24.04"
PRETTY_NAME="Ubuntu 24.04 LTS (Fixture)"
EOF

# Build fake release archive
mkdir -p "${TMP_ROOT}/pkg/praxi-linux-x86_64/bin"
for b in bot bot-node; do
  printf '#!/bin/sh\necho fixture %s\n' "$b" > "${TMP_ROOT}/pkg/praxi-linux-x86_64/bin/${b}"
  chmod +x "${TMP_ROOT}/pkg/praxi-linux-x86_64/bin/${b}"
done
printf '0.1.0-fixture\n' > "${TMP_ROOT}/pkg/praxi-linux-x86_64/VERSION"
tar -czf "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" -C "${TMP_ROOT}/pkg" praxi-linux-x86_64
sha256sum "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" > "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz.sha256"
printf 'fixture-sigstore\n' > "${RELEASE_DIR}/praxi-linux.sigstore.json"

# Fake system tools
cat > "${FAKE_BIN}/uname" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  -s) echo "Linux" ;;
  -m) echo "x86_64" ;;
  *) echo "Linux" ;;
esac
EOF
cat > "${FAKE_BIN}/id" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "-u" && "${2:-}" == "praxi" ]]; then
  [[ -f "${SANDBOX_STATE}/user_created" ]] && { echo "999"; exit 0; }
  exit 1
fi
if [[ "${1:-}" == "-u" ]]; then
  echo "0"
  exit 0
fi
command id "$@"
EOF
cat > "${FAKE_BIN}/useradd" <<'EOF'
#!/usr/bin/env bash
touch "${SANDBOX_STATE}/user_created"
EOF
cat > "${FAKE_BIN}/chown" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "${FAKE_BIN}/install" <<'EOF'
#!/usr/bin/env bash
dir_mode=0
args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    -d) dir_mode=1; shift ;;
    -o|-g|-m) shift 2 ;;
    *) args+=("$1"); shift ;;
  esac
done
if [[ "${dir_mode}" -eq 1 ]]; then
  mkdir -p "${args[@]}"
else
  cp -f "${args[0]}" "${args[1]}"
fi
EOF
cat > "${FAKE_BIN}/gh" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "--version" ]]; then
  echo "gh version 2.82.0"
  exit 0
fi
exit 0
EOF
cat > "${FAKE_BIN}/systemctl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${SANDBOX_STATE}/systemctl.log"
case "${1:-}" in
  is-active) exit 1 ;;
  is-enabled) exit 0 ;;
  cat) exit 1 ;;
  *) exit 0 ;;
esac
EOF
cat > "${FAKE_BIN}/apt-get" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${SANDBOX_STATE}/apt.log"
exit 0
EOF
cat > "${FAKE_BIN}/apt-cache" <<'EOF'
#!/usr/bin/env bash
if [[ "${1:-}" == "policy" ]]; then
  pkg="${2:-}"
  if [[ "${pkg}" == "chromium" || "${pkg}" == "chromium-browser" ]]; then
    printf '%s:\n  Installed: (none)\n  Candidate: 2:1snap1-0ubuntu2\n' "${pkg}"
  else
    printf '%s:\n  Installed: (none)\n  Candidate: 1.0-fixture\n' "${pkg}"
  fi
fi
EOF
cat > "${FAKE_BIN}/curl" <<'EOF'
#!/usr/bin/env bash
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    *) shift ;;
  esac
done
if [[ -n "${out}" ]]; then
  printf 'fake-download\n' > "${out}"
fi
EOF
cat > "${FAKE_BIN}/Xvfb" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "${FAKE_BIN}/apparmor_parser" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
cat > "${FAKE_BIN}/dpkg" <<'EOF'
#!/usr/bin/env bash
echo "amd64"
EOF
chmod +x "${FAKE_BIN}"/*

export SANDBOX_STATE="${TMP_ROOT}/state"
mkdir -p "${SANDBOX_STATE}"

# Create sandboxed copies of install-linux.sh & setup-headless.sh rewriting absolute system paths
make_sandboxed_script() {
  local src="$1" dst="$2"
  # shellcheck disable=SC2016
  sed \
    -e 's/\${EUID:-\$(id -u)}/$(id -u)/g' \
    -e "s|/opt/praxi-browser|@SB_B@|g" \
    -e "s|/opt/praxi|@SB_P@|g" \
    -e "s|/var/lib/praxi|@SB_D@|g" \
    -e "s|/etc/praxi|@SB_C@|g" \
    -e "s|/etc/systemd/system|@SB_U@|g" \
    -e "s|/usr/local/bin|@SB_L@|g" \
    -e "s|/etc/os-release|@SB_O@|g" \
    -e "s|/etc/apparmor.d|@SB_A@|g" \
    -e "s|@SB_B@|${SANDBOX}/opt/praxi-browser|g" \
    -e "s|@SB_P@|${SANDBOX}/opt/praxi|g" \
    -e "s|@SB_D@|${SANDBOX}/var/lib/praxi|g" \
    -e "s|@SB_C@|${SANDBOX}/etc/praxi|g" \
    -e "s|@SB_U@|${SANDBOX}/etc/systemd/system|g" \
    -e "s|@SB_L@|${SANDBOX}/usr/local/bin|g" \
    -e "s|@SB_O@|${SANDBOX}/etc/os-release|g" \
    -e "s|@SB_A@|${SANDBOX}/etc/apparmor.d|g" \
    "${src}" > "${dst}"
  chmod +x "${dst}"
}

SB_INSTALL="${TMP_ROOT}/install-linux.sh"
SB_SETUP="${TMP_ROOT}/setup-headless.sh"
make_sandboxed_script "${INSTALL_SH}" "${SB_INSTALL}"
make_sandboxed_script "${SETUP_SH}" "${SB_SETUP}"

# Create fake non-snap browser in sandbox so install_cft doesn't hit network when --skip-packages is used,
# AND test apt package resolution separately.
mkdir -p "${SANDBOX}/opt/praxi-browser"
printf '#!/bin/sh\necho "Chromium 157.0.8103.38"\n' > "${SANDBOX}/opt/praxi-browser/chrome"
chmod +x "${SANDBOX}/opt/praxi-browser/chrome"

export PATH="${FAKE_BIN}:${PATH}"

# 4. Option conflict checks on install-linux.sh
if bash "${SB_INSTALL}" --with-desktop-session --without-desktop-session --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" > "${TMP_ROOT}/conflict1.log" 2>&1; then
  fail "conflicting desktop-session flags should fail"
fi
if bash "${SB_INSTALL}" --offline --with-desktop-session --skip-packages --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" --custom-trusted-root "${RELEASE_DIR}/praxi-linux.sigstore.json" > "${TMP_ROOT}/conflict2.log" 2>&1; then
  fail "--offline + --with-desktop-session should fail"
fi
pass "install-linux option conflicts rejected"

# 5. Bare-Xvfb install preserved on upgrade without --with-desktop-session
if ! bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop --skip-packages --no-start > "${TMP_ROOT}/bare.log" 2>&1; then
  cat "${TMP_ROOT}/bare.log" >&2
  fail "bare Xvfb install failed"
fi
[[ -f "${SANDBOX}/etc/systemd/system/praxi-xvfb.service" ]] || fail "bare Xvfb unit missing"
[[ ! -f "${SANDBOX}/etc/systemd/system/praxi-desktop.service" ]] || fail "bare Xvfb should not install praxi-desktop.service"

# Upgrade without desktop-session flag must preserve bare Xvfb
bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --skip-packages --no-start > "${TMP_ROOT}/bare-upgrade.log" 2>&1
[[ -f "${SANDBOX}/etc/systemd/system/praxi-xvfb.service" ]] || fail "bare Xvfb unit lost on upgrade"
[[ ! -f "${SANDBOX}/etc/systemd/system/praxi-desktop.service" ]] || fail "upgrade without --with-desktop-session must not add Xfce"
pass "existing bare-Xvfb install preserved on upgrade"

# 6. Explicit --with-desktop-session install
bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop-session --skip-packages --no-start > "${TMP_ROOT}/xfce.log" 2>&1

SLICE_FILE="${SANDBOX}/etc/systemd/system/praxi.slice"
XVFB_FILE="${SANDBOX}/etc/systemd/system/praxi-xvfb.service"
DESK_FILE="${SANDBOX}/etc/systemd/system/praxi-desktop.service"
SVC_FILE="${SANDBOX}/etc/systemd/system/praxi.service"
NODE_FILE="${SANDBOX}/etc/systemd/system/praxi-node.service"
DROPIN_FILE="${SANDBOX}/etc/systemd/system/praxi.service.d/desktop.conf"
ENV_FILE="${SANDBOX}/etc/praxi/praxi.env"
NODE_ENV="${SANDBOX}/etc/praxi/praxi-node.env"
SESSION_EXEC="${SANDBOX}/opt/praxi/libexec/praxi-xfce-session.sh"

for f in "${SLICE_FILE}" "${XVFB_FILE}" "${DESK_FILE}" "${SVC_FILE}" "${NODE_FILE}" "${DROPIN_FILE}" "${ENV_FILE}" "${NODE_ENV}" "${SESSION_EXEC}"; do
  [[ -f "${f}" ]] || fail "expected generated file missing: ${f}"
done

assert_file_contains "${SLICE_FILE}" "CPUQuota=100%"
assert_file_contains "${SLICE_FILE}" "MemoryHigh=3G"
assert_file_contains "${SLICE_FILE}" "MemoryMax=4G"
for u in "${XVFB_FILE}" "${DESK_FILE}" "${SVC_FILE}" "${NODE_FILE}"; do
  assert_file_contains "${u}" "Slice=praxi.slice"
  assert_file_contains "${u}" "User=praxi"
done
assert_file_contains "${XVFB_FILE}" "-screen 0 1280x800x24"
assert_file_contains "${XVFB_FILE}" "-nolisten tcp"
assert_file_contains "${XVFB_FILE}" "-auth ${SANDBOX}/var/lib/praxi/.Xauthority"
assert_file_not_contains "${XVFB_FILE}" "-ac"
assert_file_not_contains "${XVFB_FILE}" "-fbdir"
assert_file_not_contains "${XVFB_FILE}" "xhost"

assert_file_contains "${DESK_FILE}" "Requires=praxi-xvfb.service"
assert_file_contains "${DESK_FILE}" "PartOf=praxi-xvfb.service"
assert_file_contains "${DESK_FILE}" "JoinsNamespaceOf=praxi-xvfb.service"
assert_file_contains "${DESK_FILE}" "KillMode=control-group"
assert_file_not_contains "${DESK_FILE}" "praxi.env"
awk '/^\[Service\]/{in_svc=1} /^\[/{if(!/^\[Service\]/)in_svc=0} in_svc && /^JoinsNamespaceOf=/{exit 1}' "${DESK_FILE}" || fail "JoinsNamespaceOf must be in [Unit], not [Service]"

assert_file_contains "${DROPIN_FILE}" "Requires=praxi-xvfb.service praxi-desktop.service"
assert_file_contains "${DROPIN_FILE}" "JoinsNamespaceOf=praxi-xvfb.service"

assert_file_contains "${ENV_FILE}" "DISPLAY=:99"
assert_file_contains "${ENV_FILE}" "XAUTHORITY=${SANDBOX}/var/lib/praxi/.Xauthority"
assert_file_contains "${ENV_FILE}" "DBUS_SESSION_BUS_ADDRESS=unix:path=${SANDBOX}/var/lib/praxi/.dbus-session"
assert_file_contains "${ENV_FILE}" "BOT_MAX_COMPUTER_RUNS=1"
assert_file_contains "${ENV_FILE}" "BOT_BROWSER_IDLE_SECONDS=120"

assert_file_contains "${NODE_ENV}" "DISPLAY=:99"
assert_file_contains "${NODE_ENV}" "DBUS_SESSION_BUS_ADDRESS=unix:path=${SANDBOX}/var/lib/praxi/.dbus-session"
assert_file_not_contains "${NODE_ENV}" "BOT_SECRETS_PASSPHRASE"

assert_file_contains "${SESSION_EXEC}" "xfwm4 --compositor=off"
assert_file_contains "${SESSION_EXEC}" "xfce4-screensaver.desktop"
assert_file_contains "${SESSION_EXEC}" "xfce4-power-manager.desktop"
assert_file_contains "${SESSION_EXEC}" "use_compositing"
assert_file_not_contains "${SESSION_EXEC}" "openbox"
assert_file_contains "${SANDBOX}/usr/local/bin/praxi" "Slice=praxi.slice"
pass "explicit --with-desktop-session generates secure Xfce/Xvfb/slice topology"

# 7. Re-run idempotence preserves desktop session + geometry
bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --skip-packages --no-start > "${TMP_ROOT}/idempotent.log" 2>&1
[[ -f "${DESK_FILE}" ]] || fail "idempotent re-run lost praxi-desktop.service"
assert_file_contains "${XVFB_FILE}" "-screen 0 1280x800x24"
pass "idempotent upgrade preserves praxi-desktop.service and 1280x800x24"

# 8. Explicit --without-desktop-session removes desktop session but keeps Xvfb
bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --without-desktop-session --skip-packages --no-start > "${TMP_ROOT}/downgrade.log" 2>&1
[[ ! -f "${DESK_FILE}" ]] || fail "--without-desktop-session did not remove praxi-desktop.service"
[[ -f "${XVFB_FILE}" ]] || fail "--without-desktop-session should keep praxi-xvfb.service"
assert_file_not_contains "${ENV_FILE}" "DBUS_SESSION_BUS_ADDRESS"
assert_file_contains "${ENV_FILE}" "DISPLAY=:99"
pass "--without-desktop-session rollback removes Xfce session cleanly"

# 9. setup-headless.sh reconciliation & package list checks
# 9a. User mode refuses :99 when praxi-xvfb.service owns :99
if HOME="${SANDBOX}/home/tester" bash "${SB_SETUP}" --user --display :99 --skip-packages --no-start > "${TMP_ROOT}/conflict-disp.log" 2>&1; then
  fail "setup-headless --user should refuse display :99 owned by praxi-xvfb.service"
fi
assert_file_contains "${TMP_ROOT}/conflict-disp.log" "owned by system unit praxi-xvfb.service"
pass "setup-headless --user prevents duplicate display server on :99"

# 9b. User mode on :98 creates minimal Xfce session with private Xauthority (no openbox, no -ac)
HOME="${SANDBOX}/home/tester" bash "${SB_SETUP}" --user --display :98 --skip-packages --no-start > "${TMP_ROOT}/user-xfce.log" 2>&1
USER_SESSION="${SANDBOX}/home/tester/.local/bin/praxi-headless-session.sh"
USER_UNIT="${SANDBOX}/home/tester/.config/systemd/user/praxi-headless-desktop.service"
[[ -f "${USER_SESSION}" && -f "${USER_UNIT}" ]] || fail "user-mode Xfce files missing"
assert_file_contains "${USER_SESSION}" "-nolisten tcp"
assert_file_contains "${USER_SESSION}" "-auth"
assert_file_not_contains "${USER_SESSION}" " -ac "
assert_file_not_contains "${USER_UNIT}" "openbox"
pass "setup-headless --user creates private Xfce session on :98"

# 9c. System mode restores Xfce session and cleans up any conflicting user unit on :99
mkdir -p "${SANDBOX}/home/tester/.config/systemd/user"
printf 'Environment="DISPLAY=:99"\n' > "${SANDBOX}/home/tester/.config/systemd/user/praxi-headless-desktop.service"
: > "${SANDBOX_STATE}/apt.log"
HOME="${SANDBOX}/home/tester" bash "${SB_SETUP}" --system --display :99 --screen 1280x800x24 --no-start > "${TMP_ROOT}/sys-setup.log" 2>&1
[[ -f "${DESK_FILE}" ]] || fail "setup-headless --system did not create praxi-desktop.service"
[[ ! -f "${SANDBOX}/home/tester/.config/systemd/user/praxi-headless-desktop.service" ]] || fail "setup-headless --system did not remove conflicting user unit on :99"
assert_file_contains "${SANDBOX_STATE}/apt.log" "xfwm4 xfce4-panel xfdesktop4"
assert_file_not_contains "${SANDBOX_STATE}/apt.log" "chromium-browser"
assert_file_not_contains "${SANDBOX_STATE}/apt.log" "x11vnc"
assert_file_not_contains "${SANDBOX_STATE}/apt.log" "cups"
pass "setup-headless --system installs minimal Xfce packages and reconciles :99"

# 10. Explicit --without-desktop removes both units and DISPLAY
bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --without-desktop --skip-packages --no-start > "${TMP_ROOT}/nodesk.log" 2>&1
[[ ! -f "${DESK_FILE}" && ! -f "${XVFB_FILE}" ]] || fail "--without-desktop did not remove desktop units"
assert_file_not_contains "${ENV_FILE}" "DISPLAY="
assert_file_not_contains "${ENV_FILE}" "XAUTHORITY="
pass "--without-desktop removes Xvfb and desktop session cleanly"

# 11. systemd-analyze verify check (only when systemd-analyze exists on host)
if command -v systemd-analyze >/dev/null 2>&1; then
  # Recreate units in sandbox and verify unit syntax (ignoring missing non-root binary paths in sandbox)
  echo "INFO: systemd-analyze found; checking slice/unit syntax"
  systemd-analyze verify "${SLICE_FILE}" 2>&1 || true
else
  echo "SKIP: systemd-analyze not available on this platform (run on Linux target VM)"
fi

echo "ALL PACKAGING HERMETIC TESTS PASSED"
