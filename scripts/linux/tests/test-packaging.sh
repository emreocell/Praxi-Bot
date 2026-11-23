#!/usr/bin/env bash
# Hermetic tests for scripts/install-linux.sh and scripts/linux/setup-headless.sh.
# Uses temporary directories and fake package/systemctl/gh/curl shims.

set -euo pipefail

# This test puts fake commands (id, uname, systemctl, ...) on PATH and drives
# the installers against a sandbox. It is only meaningful, and only safe, on a
# real Linux host (CI); on Git Bash/MSYS a shim chain once spawned thousands of
# processes. Refuse to run anywhere else instead of guessing.
if [[ "${OSTYPE:-}" != linux* ]]; then
  echo "SKIP: test-packaging.sh runs only on Linux (OSTYPE=${OSTYPE:-unset}); use CI" >&2
  exit 0
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
INSTALL_SH="${REPO_ROOT}/scripts/install-linux.sh"
SETUP_SH="${REPO_ROOT}/scripts/linux/setup-headless.sh"
REAL_UID="$(id -u)"

TMP_ROOT="$(mktemp -d)"
trap 'rm -rf "${TMP_ROOT}"' EXIT

exec 3>&2
pass() { printf 'PASS: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&3; exit 1; }

assert_file_contains() {
  local file="$1" pattern="$2" msg="${3:-}"
  if ! grep -Fq -- "${pattern}" "${file}"; then
    cat "${file}" >&3 || true
    fail "${msg:-expected pattern ${pattern} in ${file}}"
  fi
}

assert_file_not_contains() {
  local file="$1" pattern="$2" msg="${3:-}"
  if grep -Fq -- "${pattern}" "${file}"; then
    fail "${msg:-did not expect pattern ${pattern} in ${file}}"
  fi
}

run_timed() {
  local rc=0
  command -v timeout >/dev/null 2>&1 || fail "timeout command is required for test-packaging.sh"
  (
    local cur_u cur_cnt target_u
    cur_u="$(ulimit -u 2>/dev/null || echo unlimited)"
    cur_cnt="$({ ps -u "${REAL_UID}" --no-headers 2>/dev/null || true; } | wc -l)"
    cur_cnt="${cur_cnt//[^0-9]/}"
    target_u=$(( ${cur_cnt:-0} + 128 ))
    if [[ "${cur_u}" == "unlimited" ]] || { [[ "${cur_u}" =~ ^[0-9]+$ ]] && (( cur_u > target_u )); }; then
      ulimit -u "${target_u}" 2>/dev/null || true
    fi
    exec timeout -k 5s 30s "$@"
  ) || rc=$?
  if [[ "${rc}" -eq 124 || "${rc}" -eq 137 ]]; then
    fail "command timed out (rc=${rc}): $*"
  fi
  return "${rc}"
}
run_ok() {
  local logfile="$1"; shift
  if ! run_timed "$@" > "${logfile}" 2>&1; then
    cat "${logfile}" >&3
    fail "command failed: $*"
  fi
}

# 1. Syntax & lint checks
run_timed bash -n "${INSTALL_SH}" "${SETUP_SH}" "$0"
pass "bash -n syntax checks"
if command -v shellcheck >/dev/null 2>&1; then
  run_timed shellcheck -x "${INSTALL_SH}" "${SETUP_SH}" "$0"
  pass "shellcheck -x clean"
else
  echo "SKIP: shellcheck not installed"
fi

# 2. Dry-run & CLI validation checks
run_timed "${SETUP_SH}" --dry-run --user --display :98 --screen 1280x800x24 > "${TMP_ROOT}/dry-user.log"
assert_file_contains "${TMP_ROOT}/dry-user.log" "mode=user session=xfce without_desktop=0 display=:98 screen=1280x800x24"
run_timed "${SETUP_SH}" --dry-run --system --display :99 --screen 1280x800x24 > "${TMP_ROOT}/dry-sys.log"
assert_file_contains "${TMP_ROOT}/dry-sys.log" "CPUQuota=100%, MemoryHigh=3G, MemoryMax=4G"

if run_timed "${SETUP_SH}" --dry-run --vnc > "${TMP_ROOT}/vnc.log" 2>&1; then
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
#
# Recursion safety: every shim shares ONE exported depth counter (a per-name
# counter lets uname -> id -> uname chains multiply) and a global invocation
# budget kept in a file, so neither a self-calling shim nor a fan-out of shims
# can spawn processes without bound. The shebang is an absolute bash path so
# starting a shim never does a PATH lookup that could land on another shim.
BASH_BIN="$(command -v bash)"
[[ -x "${BASH_BIN}" ]] || fail "could not resolve bash binary"
SHIM_MAX_DEPTH=6
SHIM_MAX_CALLS=5000
SHIM_BUDGET_FILE="${TMP_ROOT}/shim-budget"
: > "${SHIM_BUDGET_FILE}"
# The shim text is emitted with printf on purpose: the single-quoted
# expansions must reach the generated file unexpanded.
# shellcheck disable=SC2016
write_fake_shim() {
  local name="$1"
  {
    printf '#!%s\n' "${BASH_BIN}"
    printf '_d="${_PRAXI_FAKE_DEPTH:-0}"\n'
    printf 'if [[ "${_d}" -ge %s ]]; then\n' "${SHIM_MAX_DEPTH}"
    printf '  echo "ERROR: fake %s recursion guard triggered (depth=${_d})" >&2\n' "${name}"
    printf '  exit 99\n'
    printf 'fi\n'
    printf 'printf x >> "%s"\n' "${SHIM_BUDGET_FILE}"
    printf '_c="$(wc -c < "%s")"\n' "${SHIM_BUDGET_FILE}"
    printf 'if [[ "${_c//[^0-9]/}" -ge %s ]]; then\n' "${SHIM_MAX_CALLS}"
    printf '  echo "ERROR: fake %s invocation budget exhausted" >&2\n' "${name}"
    printf '  exit 99\n'
    printf 'fi\n'
    printf 'export _PRAXI_FAKE_DEPTH=$((_d + 1))\n'
    cat
  } > "${FAKE_BIN}/${name}"
  chmod +x "${FAKE_BIN}/${name}"
}

write_fake_shim uname <<'EOF'
case "${1:-}" in
  -s) echo "Linux" ;;
  -m) echo "x86_64" ;;
  *) echo "Linux" ;;
esac
EOF
REAL_ID="$(command -v id)"
[[ -x "${REAL_ID}" ]] || fail "could not resolve real id binary"
# The real id must never be a shim: exec-ing a shim from the id shim is the
# unbounded self-recursion this test once suffered from.
[[ "${REAL_ID}" != "${FAKE_BIN}"/* ]] || fail "real id resolved inside FAKE_BIN (${REAL_ID})"
write_fake_shim id <<EOF
if [[ "\${1:-}" == "-u" && "\${2:-}" == "praxi" ]]; then
  [[ -f "\${SANDBOX_STATE}/user_created" ]] && { echo "999"; exit 0; }
  exit 1
fi
if [[ "\${1:-}" == "-u" && "\${2:-}" == "tester" ]]; then
  echo "1000"
  exit 0
fi
if [[ "\${1:-}" == "-u" && -z "\${2:-}" ]]; then
  echo "\${FAKE_UID:-0}"
  exit 0
fi
exec "${REAL_ID}" "\$@"
EOF
write_fake_shim useradd <<'EOF'
touch "${SANDBOX_STATE}/user_created"
EOF
write_fake_shim chown <<'EOF'
printf 'chown %s\n' "$*" >> "${SANDBOX_STATE}/perms.log"
exit 0
EOF
write_fake_shim install <<'EOF'
dir_mode=0
owner=""
group=""
mode=""
args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    -d) dir_mode=1; shift ;;
    -o) owner="$2"; shift 2 ;;
    -g) group="$2"; shift 2 ;;
    -m) mode="$2"; shift 2 ;;
    *) args+=("$1"); shift ;;
  esac
done
if [[ "${dir_mode}" -eq 1 ]]; then
  if [[ -n "${FAKE_FAIL_INSTALL_DIR_MATCH:-}" && "${args[*]}" == *"${FAKE_FAIL_INSTALL_DIR_MATCH}"* ]]; then
    echo "install: cannot create directory '${args[*]}': No space left on device" >&2
    exit 1
  fi
  mkdir -p "${args[@]}"
  if [[ -n "${mode}" ]]; then
    chmod "${mode}" "${args[@]}"
  fi
  printf 'install -d owner=%s group=%s mode=%s %s\n' "${owner}" "${group}" "${mode}" "${args[*]}" >> "${SANDBOX_STATE}/perms.log"
else
  dst="${args[1]}"
  base="$(basename "${dst}")"
  if [[ -n "${FAKE_SLEEP_BEFORE_INSTALL_TARGET:-}" && "${base}" == "${FAKE_SLEEP_BEFORE_INSTALL_TARGET}" ]]; then
    if [[ -n "${FAKE_READY_MARKER_FILE:-}" ]]; then
      : > "${FAKE_READY_MARKER_FILE}"
    fi
    sleep 5
  fi
  if [[ -n "${FAKE_FAIL_INSTALL_TARGET:-}" && "${base}" == "${FAKE_FAIL_INSTALL_TARGET}" ]]; then
    echo "install: error writing '${dst}': No space left on device" >&2
    exit 1
  fi
  cp -f "${args[0]}" "${dst}"
  if [[ -n "${mode}" ]]; then
    chmod "${mode}" "${dst}"
  fi
  printf 'install owner=%s group=%s mode=%s %s\n' "${owner}" "${group}" "${mode}" "${dst}" >> "${SANDBOX_STATE}/perms.log"
fi
EOF
write_fake_shim gh <<'EOF'
if [[ "${1:-}" == "--version" ]]; then
  echo "gh version 2.82.0"
  exit 0
fi
exit 0
EOF
write_fake_shim systemctl <<'EOF'
printf '%s\n' "$*" >> "${SANDBOX_STATE}/systemctl.log"
if [[ "${1:-}" == "--version" ]]; then
  echo "systemd ${FAKE_SYSTEMD_VERSION:-255} (255.4-1ubuntu8.18)"
  exit 0
fi
if [[ "$*" == "show --property=ExecStart --value praxi.service" ]]; then
  printf '%s\n' "${FAKE_PRAXI_EXEC_START:-}"
  exit 0
fi
verb=""
has_now=0
units=()
for a in "$@"; do
  case "${a}" in
    --user|--quiet|--no-block) continue ;;
    --now) has_now=1; continue ;;
    *)
      if [[ -z "${verb}" ]]; then
        verb="${a}"
      else
        units+=("${a}")
      fi
      ;;
  esac
done
unit="${units[0]:-}"
state_add() {
  local f="$1" u="$2"
  touch "${f}"
  if ! grep -Fxq -- "${u}" "${f}" 2>/dev/null; then
    printf '%s\n' "${u}" >> "${f}"
  fi
}
state_del() {
  local f="$1" u="$2"
  if [[ -f "${f}" ]]; then
    grep -Fxv -- "${u}" "${f}" > "${f}.tmp" 2>/dev/null || true
    mv -f "${f}.tmp" "${f}"
  fi
}
case "${verb}" in
  is-active)
    if [[ "${FAKE_STATEFUL_SYSTEMCTL:-0}" -eq 1 ]]; then
      [[ -n "${unit}" ]] && grep -Fxq -- "${unit}" "${SANDBOX_STATE}/active_units" 2>/dev/null && exit 0
      exit 1
    fi
    if [[ -n "${unit}" && " ${FAKE_ACTIVE_UNITS:-} " == *" ${unit} "* ]]; then
      exit 0
    fi
    exit 1
    ;;
  is-enabled)
    if [[ "${FAKE_STATEFUL_SYSTEMCTL:-0}" -eq 1 ]]; then
      [[ -n "${unit}" ]] && grep -Fxq -- "${unit}" "${SANDBOX_STATE}/enabled_units" 2>/dev/null && exit 0
      exit 1
    fi
    exit 0
    ;;
  cat) exit 1 ;;
  start|restart)
    for u in "${units[@]}"; do
      if [[ -n "${FAKE_FAIL_START_UNIT:-}" && "${u}" == "${FAKE_FAIL_START_UNIT}" && -f "${FAKE_FAIL_ONCE_FLAG:-/nonexistent}" ]]; then
        rm -f "${FAKE_FAIL_ONCE_FLAG}"
        exit 1
      fi
      if [[ "${FAKE_STATEFUL_SYSTEMCTL:-0}" -eq 1 ]]; then
        state_add "${SANDBOX_STATE}/active_units" "${u}"
      fi
    done
    exit 0
    ;;
  stop)
    if [[ "${FAKE_STATEFUL_SYSTEMCTL:-0}" -eq 1 ]]; then
      for u in "${units[@]}"; do
        state_del "${SANDBOX_STATE}/active_units" "${u}"
      done
    fi
    exit 0
    ;;
  enable)
    if [[ "${FAKE_STATEFUL_SYSTEMCTL:-0}" -eq 1 ]]; then
      for u in "${units[@]}"; do
        state_add "${SANDBOX_STATE}/enabled_units" "${u}"
        if [[ "${has_now}" -eq 1 ]]; then
          state_add "${SANDBOX_STATE}/active_units" "${u}"
        fi
      done
    fi
    exit 0
    ;;
  disable)
    if [[ "${FAKE_STATEFUL_SYSTEMCTL:-0}" -eq 1 ]]; then
      for u in "${units[@]}"; do
        state_del "${SANDBOX_STATE}/enabled_units" "${u}"
        if [[ "${has_now}" -eq 1 ]]; then
          state_del "${SANDBOX_STATE}/active_units" "${u}"
        fi
      done
    fi
    exit 0
    ;;
  *) exit 0 ;;
esac
EOF
write_fake_shim apt-get <<'EOF'
if [[ "${DEBIAN_FRONTEND:-}" != noninteractive || "${NEEDRESTART_MODE:-}" != l || "${NEEDRESTART_SUSPEND:-}" != 1 ]]; then
  echo "apt-get must suspend needrestart and run noninteractively" >&2
  exit 1
fi
printf '%s\n' "$*" >> "${SANDBOX_STATE}/apt.log"
if [[ "${FAKE_FAIL_APT:-0}" -eq 1 && "${1:-}" == "install" ]]; then
  echo "E: Unable to fetch some archives (simulated package failure)" >&2
  exit 100
fi
exit 0
EOF
write_fake_shim apt-cache <<'EOF'
if [[ "${1:-}" == "policy" ]]; then
  pkg="${2:-}"
  if [[ "${pkg}" == "chromium" || "${pkg}" == "chromium-browser" ]]; then
    printf '%s:\n  Installed: (none)\n  Candidate: %s\n' "${pkg}" "${FAKE_CHROMIUM_CANDIDATE:-2:1snap1-0ubuntu2}"
  else
    printf '%s:\n  Installed: (none)\n  Candidate: 1.0-fixture\n' "${pkg}"
  fi
fi
EOF
write_fake_shim curl <<'EOF'
out=""
url=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    --connect-timeout|--max-time|--noproxy) shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
if [[ -n "${out}" ]]; then
  printf 'fake-download\n' > "${out}"
elif [[ "${url}" == *"/v1/health"* ]]; then
  if [[ "${FAKE_FAIL_HEALTH:-0}" -eq 1 ]]; then
    exit 22
  fi
  printf '{"status":"ok","version":"0.1.0"}\n'
fi
EOF
write_fake_shim journalctl <<'EOF'
exit 0
EOF
write_fake_shim Xvfb <<'EOF'
exit 0
EOF
write_fake_shim xdpyinfo <<'EOF'
exit 0
EOF
write_fake_shim apparmor_parser <<'EOF'
exit 0
EOF
write_fake_shim dpkg <<'EOF'
echo "amd64"
EOF
write_fake_shim xauth <<'EOF'
printf '%s\n' "$*" >> "${SANDBOX_STATE}/xauth.log"
file=""
if [[ "${1:-}" == "-f" ]]; then
  file="$2"
  shift 2
fi
case "${1:-}" in
  list)
    disp="${2:-}"
    if [[ -s "${file}" ]] && grep -Fq "${disp}" "${file}"; then
      printf 'host/unix%s  MIT-MAGIC-COOKIE-1  deadbeef\n' "${disp}"
    fi
    ;;
  add)
    printf '%s %s %s\n' "${2:-}" "${3:-}" "${4:-}" >> "${file}"
    ;;
esac
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
    -e "s|/etc/systemd/journald.conf.d|@SB_J@|g" \
    -e "s|@SB_B@|${SANDBOX}/opt/praxi-browser|g" \
    -e "s|@SB_P@|${SANDBOX}/opt/praxi|g" \
    -e "s|@SB_D@|${SANDBOX}/var/lib/praxi|g" \
    -e "s|@SB_C@|${SANDBOX}/etc/praxi|g" \
    -e "s|@SB_U@|${SANDBOX}/etc/systemd/system|g" \
    -e "s|@SB_L@|${SANDBOX}/usr/local/bin|g" \
    -e "s|@SB_O@|${SANDBOX}/etc/os-release|g" \
    -e "s|@SB_A@|${SANDBOX}/etc/apparmor.d|g" \
    -e "s|@SB_J@|${SANDBOX}/etc/systemd/journald.conf.d|g" \
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
if run_timed bash "${SB_INSTALL}" --with-desktop-session --without-desktop-session --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" > "${TMP_ROOT}/conflict1.log" 2>&1; then
  fail "conflicting desktop-session flags should fail"
fi
if run_timed bash "${SB_INSTALL}" --offline --with-desktop-session --skip-packages --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" --custom-trusted-root "${RELEASE_DIR}/praxi-linux.sigstore.json" > "${TMP_ROOT}/conflict2.log" 2>&1; then
  fail "--offline + --with-desktop-session should fail"
fi
pass "install-linux option conflicts rejected"

# 4b. Verify FAKE_BIN recursion guard trips immediately instead of fork-bombing
if _PRAXI_FAKE_DEPTH="${SHIM_MAX_DEPTH}" "${FAKE_BIN}/id" -u >/dev/null 2>&1; then
  fail "fake id recursion guard did not trigger at depth=${SHIM_MAX_DEPTH}"
fi
rc=0
_PRAXI_FAKE_DEPTH="${SHIM_MAX_DEPTH}" "${FAKE_BIN}/systemctl" is-active praxi.service >/dev/null 2>&1 || rc=$?
[[ "${rc}" -eq 99 ]] || fail "expected exit 99 from recursion guard, got ${rc}"
# A shim must export its incremented depth to children, and must refuse to run
# once the global call budget is spent.
write_fake_shim probe <<'PROBE'
bash -c 'printf "%s" "${_PRAXI_FAKE_DEPTH:-unset}"'
PROBE
probe_depth="$("${FAKE_BIN}/probe")"
[[ "${probe_depth}" == "1" ]] || fail "shim did not export _PRAXI_FAKE_DEPTH=1 to its child (got ${probe_depth})"
head -c "${SHIM_MAX_CALLS}" /dev/zero > "${SHIM_BUDGET_FILE}"
rc=0
"${FAKE_BIN}/probe" >/dev/null 2>&1 || rc=$?
[[ "${rc}" -eq 99 ]] || fail "expected exit 99 once the shim call budget is spent, got ${rc}"
: > "${SHIM_BUDGET_FILE}"
rm -f "${FAKE_BIN}/probe"
pass "FAKE_BIN recursion guard prevents runaway recursion"

# 5. Legacy bare-Xvfb (origin/main without -auth / xdpyinfo) preserved on no-flag upgrade,
#    praxi.env omits active limit overrides by default and preserves user overrides,
#    and missing Xvfb/xauth/xdpyinfo aborts pre-flight before stopping active services.
cat > "${SANDBOX}/etc/praxi/praxi.env" <<EOF
BOT_DATA_DIR=${SANDBOX}/var/lib/praxi
BOT_PORT=47821
BOT_SECRETS_BACKEND=file
BOT_SECRETS_PASSPHRASE=legacy-passphrase
HOME=${SANDBOX}/var/lib/praxi
DISPLAY=:99
EOF
cat > "${SANDBOX}/etc/systemd/system/praxi-xvfb.service" <<EOF
[Unit]
Description=Praxi virtual display (Xvfb :99)

[Service]
Type=simple
User=praxi
Group=praxi
ExecStartPre=!/usr/bin/install -d -o root -g root -m 1777 /tmp/.X11-unix
ExecStart=${FAKE_BIN}/Xvfb :99 -screen 0 1920x1080x24 -nolisten tcp +extension RANDR +extension XTEST
Restart=on-failure
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF

# 5a. Pre-flight check fails BEFORE stopping active services when xdpyinfo is missing
PREFLIGHT_BIN="${TMP_ROOT}/preflight-bin"
NO_X_HOST="${TMP_ROOT}/no-x-host"
rm -rf "${PREFLIGHT_BIN}" "${NO_X_HOST}"
mkdir -p "${PREFLIGHT_BIN}" "${NO_X_HOST}"
python3 -c '
import os, sys
dst = sys.argv[1]
for d in ("/usr/bin", "/bin"):
    if not os.path.isdir(d):
        continue
    for name in os.listdir(d):
        if name == "xdpyinfo":
            continue
        p = os.path.join(dst, name)
        if not os.path.lexists(p):
            os.symlink(os.path.join(d, name), p)
' "${NO_X_HOST}"
for b in "${FAKE_BIN}"/*; do
  [[ "$(basename "${b}")" == "xdpyinfo" ]] && continue
  ln -sf "${b}" "${PREFLIGHT_BIN}/$(basename "${b}")"
done
: > "${SANDBOX_STATE}/systemctl.log"
if PATH="${PREFLIGHT_BIN}:${NO_X_HOST}" FAKE_ACTIVE_UNITS="praxi-xvfb.service praxi.service" \
  run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop --skip-packages --no-start > "${TMP_ROOT}/preflight-fail.log" 2>&1; then
  fail "expected pre-flight check to fail when xdpyinfo is missing"
fi
assert_file_contains "${TMP_ROOT}/preflight-fail.log" "xdpyinfo"
assert_file_not_contains "${SANDBOX_STATE}/systemctl.log" "stop praxi"
pass "pre-flight check fails before stopping active services when xdpyinfo is missing"

# 5b. No-flag legacy upgrade preserves bare Xvfb unit, emits warning, and does not inject limit overrides
run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --skip-packages --no-start > "${TMP_ROOT}/legacy-upgrade.log" 2>&1
[[ -f "${SANDBOX}/etc/systemd/system/praxi-xvfb.service" ]] || fail "bare Xvfb unit lost on upgrade"
[[ ! -f "${SANDBOX}/etc/systemd/system/praxi-desktop.service" ]] || fail "upgrade without --with-desktop-session must not add Xfce"
assert_file_not_contains "${SANDBOX}/etc/systemd/system/praxi-xvfb.service" "praxi-wait-x11.sh"
assert_file_not_contains "${SANDBOX}/etc/systemd/system/praxi-xvfb.service" "-auth"
assert_file_not_contains "${SANDBOX}/etc/praxi/praxi.env" "XAUTHORITY="
assert_file_not_contains "${SANDBOX}/etc/praxi/praxi.env" "BOT_MAX_COMPUTER_RUNS="
assert_file_not_contains "${SANDBOX}/etc/praxi/praxi.env" "BOT_BROWSER_IDLE_SECONDS="
assert_file_contains "${TMP_ROOT}/legacy-upgrade.log" "keeping legacy praxi-xvfb.service without -auth"

# Preserve existing user overrides in praxi.env across upgrade
printf 'BOT_MAX_COMPUTER_RUNS=3\nBOT_BROWSER_IDLE_SECONDS=300\n' >> "${SANDBOX}/etc/praxi/praxi.env"
run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --skip-packages --no-start > "${TMP_ROOT}/override-upgrade.log" 2>&1
assert_file_contains "${SANDBOX}/etc/praxi/praxi.env" "BOT_MAX_COMPUTER_RUNS=3"
assert_file_contains "${SANDBOX}/etc/praxi/praxi.env" "BOT_BROWSER_IDLE_SECONDS=300"
assert_file_contains "${TMP_ROOT}/override-upgrade.log" "daemon is stopped (--no-start)"
FAKE_ACTIVE_UNITS="praxi.service" run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --skip-packages --no-start > "${TMP_ROOT}/nostart-active-upgrade.log" 2>&1
assert_file_contains "${TMP_ROOT}/nostart-active-upgrade.log" "stopped for upgrade and remain stopped (--no-start)"
sed -i '/^BOT_MAX_COMPUTER_RUNS=/d;/^BOT_BROWSER_IDLE_SECONDS=/d' "${SANDBOX}/etc/praxi/praxi.env"

# 5b-host. --host-mode writes 'bot serve --host' and a later plain re-install keeps it
run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz"   --skip-packages --no-start --host-mode > "${TMP_ROOT}/host-mode.log" 2>&1
assert_file_contains "${SANDBOX}/etc/systemd/system/praxi.service" "/bot serve --host"
run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz"   --skip-packages --no-start > "${TMP_ROOT}/host-mode-keep.log" 2>&1
assert_file_contains "${SANDBOX}/etc/systemd/system/praxi.service" "/bot serve --host"

# 5c. Comprehensive rollback tests across failure stages
setup_prev_install() {
  mkdir -p "${SANDBOX}/opt/praxi/bin" "${SANDBOX}/etc/systemd/system" "${SANDBOX}/etc/praxi" "${SANDBOX}/usr/local/bin"
  rm -rf "${SANDBOX}/etc/systemd/system/praxi-desktop.service" \
    "${SANDBOX}/etc/systemd/system/praxi.slice" \
    "${SANDBOX}/etc/systemd/system/praxi.service.d" \
    "${SANDBOX}/etc/systemd/system/praxi-node.service.d" \
    "${SANDBOX}/opt/praxi/libexec"
  printf 'v0.0.9-prev\n' > "${SANDBOX}/opt/praxi/VERSION"
  printf '#!/bin/sh\necho prev-bot-0.0.9\n' > "${SANDBOX}/opt/praxi/bin/bot"
  printf '#!/bin/sh\necho prev-node-0.0.9\n' > "${SANDBOX}/opt/praxi/bin/bot-node"
  chmod +x "${SANDBOX}/opt/praxi/bin/bot" "${SANDBOX}/opt/praxi/bin/bot-node"
  cat > "${SANDBOX}/etc/praxi/praxi.env" <<EOF
BOT_DATA_DIR=${SANDBOX}/var/lib/praxi
BOT_PORT=47821
BOT_SECRETS_BACKEND=file
BOT_SECRETS_PASSPHRASE=legacy-passphrase
HOME=${SANDBOX}/var/lib/praxi
DISPLAY=:99
CUSTOM_KEEP=1
EOF
  cat > "${SANDBOX}/etc/systemd/system/praxi-xvfb.service" <<EOF
[Unit]
Description=Praxi virtual display (Xvfb :99)

[Service]
Type=simple
User=praxi
Group=praxi
ExecStartPre=!/usr/bin/install -d -o root -g root -m 1777 /tmp/.X11-unix
ExecStart=${FAKE_BIN}/Xvfb :99 -screen 0 1920x1080x24 -nolisten tcp +extension RANDR +extension XTEST
Restart=on-failure
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF
  cat > "${SANDBOX}/etc/systemd/system/praxi.service" <<EOF
[Unit]
Description=Praxi-Bot daemon prev

[Service]
ExecStart=${SANDBOX}/opt/praxi/bin/bot serve
EOF
  cp -f "${SANDBOX}/etc/praxi/praxi.env" "${TMP_ROOT}/prev-praxi.env.snap"
  cp -f "${SANDBOX}/etc/systemd/system/praxi-xvfb.service" "${TMP_ROOT}/prev-xvfb.service.snap"
  cp -f "${SANDBOX}/etc/systemd/system/praxi.service" "${TMP_ROOT}/prev-praxi.service.snap"
  printf 'praxi-xvfb.service\npraxi.service\n' > "${SANDBOX_STATE}/active_units"
  printf 'praxi-xvfb.service\npraxi.service\n' > "${SANDBOX_STATE}/enabled_units"
  : > "${SANDBOX_STATE}/systemctl.log"
}

assert_prev_install_restored_and_running() {
  local label="$1"
  assert_file_contains "${SANDBOX}/opt/praxi/VERSION" "v0.0.9-prev" "${label}: VERSION not restored"
  assert_file_contains "${SANDBOX}/opt/praxi/bin/bot" "prev-bot-0.0.9" "${label}: bot binary not restored"
  diff -u "${TMP_ROOT}/prev-praxi.env.snap" "${SANDBOX}/etc/praxi/praxi.env" || fail "${label}: praxi.env bytes not restored"
  diff -u "${TMP_ROOT}/prev-xvfb.service.snap" "${SANDBOX}/etc/systemd/system/praxi-xvfb.service" || fail "${label}: praxi-xvfb.service bytes not restored"
  diff -u "${TMP_ROOT}/prev-praxi.service.snap" "${SANDBOX}/etc/systemd/system/praxi.service" || fail "${label}: praxi.service bytes not restored"
  [[ ! -f "${SANDBOX}/etc/systemd/system/praxi-desktop.service" ]] || fail "${label}: new praxi-desktop.service should be removed"
  [[ ! -f "${SANDBOX}/etc/systemd/system/praxi.slice" ]] || fail "${label}: new praxi.slice should be removed"
  [[ ! -d "${SANDBOX}/etc/systemd/system/praxi.service.d" ]] || fail "${label}: new praxi.service.d should be removed"
  grep -Fxq "praxi-xvfb.service" "${SANDBOX_STATE}/active_units" || fail "${label}: praxi-xvfb.service not active after rollback"
  grep -Fxq "praxi.service" "${SANDBOX_STATE}/active_units" || fail "${label}: praxi.service not active after rollback"
  if grep -Fxq "praxi-desktop.service" "${SANDBOX_STATE}/active_units"; then
    fail "${label}: praxi-desktop.service should not be active after rollback"
  fi
  if grep -Fxq "praxi-desktop.service" "${SANDBOX_STATE}/enabled_units"; then
    fail "${label}: praxi-desktop.service should not be enabled after rollback"
  fi
}

# 5c-1. Package-step failure aborts BEFORE stopping active services
setup_prev_install
if FAKE_STATEFUL_SYSTEMCTL=1 FAKE_FAIL_APT=1 \
  run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop-session > "${TMP_ROOT}/rollback-pkg.log" 2>&1; then
  fail "expected package install failure to exit non-zero"
fi
assert_file_not_contains "${SANDBOX_STATE}/systemctl.log" "stop praxi"
assert_file_not_contains "${SANDBOX_STATE}/systemctl.log" "restart praxi"
assert_prev_install_restored_and_running "5c-1 package failure"

# 5c-2. Disk-full (ENOSPC) failure during binary install after stopping services
setup_prev_install
if FAKE_STATEFUL_SYSTEMCTL=1 FAKE_FAIL_INSTALL_TARGET="bot" \
  run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop-session --skip-packages > "${TMP_ROOT}/rollback-enospc.log" 2>&1; then
  fail "expected binary install ENOSPC to exit non-zero"
fi
assert_file_contains "${TMP_ROOT}/rollback-enospc.log" "rolling back to previous working installation"
assert_prev_install_restored_and_running "5c-2 binary install ENOSPC"

# 5c-3. Unit write failure AFTER binaries, praxi.env, and unit files were overwritten
setup_prev_install
if FAKE_STATEFUL_SYSTEMCTL=1 FAKE_FAIL_INSTALL_DIR_MATCH="praxi.service.d" \
  run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop-session --skip-packages > "${TMP_ROOT}/rollback-unit-write.log" 2>&1; then
  fail "expected unit write failure to exit non-zero"
fi
assert_file_contains "${TMP_ROOT}/rollback-unit-write.log" "rolling back to previous working installation"
assert_prev_install_restored_and_running "5c-3 unit write failure"

# 5c-4. Failed systemctl start during upgrade
setup_prev_install
touch "${TMP_ROOT}/fail-once"
if FAKE_STATEFUL_SYSTEMCTL=1 FAKE_FAIL_START_UNIT="praxi.service" FAKE_FAIL_ONCE_FLAG="${TMP_ROOT}/fail-once" \
  run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --skip-packages > "${TMP_ROOT}/rollback-start.log" 2>&1; then
  fail "expected failed startup during upgrade to exit non-zero and trigger rollback"
fi
assert_file_contains "${TMP_ROOT}/rollback-start.log" "rolling back to previous working installation"
assert_file_not_contains "${SANDBOX_STATE}/systemctl.log" "restart praxi"
assert_prev_install_restored_and_running "5c-4 failed start"

# 5c-5. Readiness check failure AFTER new install started & enabled praxi-xvfb, praxi-desktop, praxi
setup_prev_install
if FAKE_STATEFUL_SYSTEMCTL=1 FAKE_FAIL_HEALTH=1 PRAXI_READY_TIMEOUT_SECS=2 \
  run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop-session --skip-packages > "${TMP_ROOT}/rollback-readiness.log" 2>&1; then
  fail "expected failed readiness check to exit non-zero and trigger rollback"
fi
assert_file_contains "${TMP_ROOT}/rollback-readiness.log" "rolling back to previous working installation"
assert_file_contains "${SANDBOX_STATE}/systemctl.log" "disable praxi-desktop.service"
# Verify rollback stops new praxi-xvfb/praxi-desktop/praxi AFTER readiness failure and BEFORE daemon-reload/start
python3 - "${SANDBOX_STATE}/systemctl.log" <<'PY' || fail "5c-5 rollback stop/restore/start ordering wrong"
import sys
lines = [line.strip() for line in open(sys.argv[1], encoding="utf-8")]
# Second stop of praxi-xvfb.service is the rollback stop (first is pre-install stop)
xvfb_stops = [i for i, l in enumerate(lines) if l == "stop praxi-xvfb.service"]
assert len(xvfb_stops) >= 2, f"expected >=2 stop praxi-xvfb.service, got {lines}"
rb_xvfb_stop = xvfb_stops[-1]
rb_desk_stop = max(i for i, l in enumerate(lines) if l == "stop praxi-desktop.service")
rb_praxi_stop = max(i for i, l in enumerate(lines) if l == "stop praxi.service")
rb_reload = max(i for i, l in enumerate(lines) if l == "daemon-reload")
rb_praxi_start = max(i for i, l in enumerate(lines) if l == "start praxi.service")
assert rb_xvfb_stop < rb_desk_stop < rb_praxi_stop < rb_reload < rb_praxi_start, lines[rb_xvfb_stop:]
PY
assert_prev_install_restored_and_running "5c-5 readiness check failure"

# 5c-6. Signal (SIGTERM) mid-install after services were stopped triggers rollback and exits non-zero
setup_prev_install
rm -f "${TMP_ROOT}/mid-install-marker"
FAKE_STATEFUL_SYSTEMCTL=1 \
  FAKE_SLEEP_BEFORE_INSTALL_TARGET="bot" \
  FAKE_READY_MARKER_FILE="${TMP_ROOT}/mid-install-marker" \
  timeout -k 5s 30s bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop-session --skip-packages > "${TMP_ROOT}/rollback-signal.log" 2>&1 &
sig_pid=$!
for _ in $(seq 1 100); do
  [[ -f "${TMP_ROOT}/mid-install-marker" ]] && break
  sleep 0.05
done
[[ -f "${TMP_ROOT}/mid-install-marker" ]] || { kill -KILL "${sig_pid}" 2>/dev/null || true; fail "5c-6 did not reach mid-install marker"; }
kill -TERM "${sig_pid}"
sig_rc=0
wait "${sig_pid}" || sig_rc=$?
[[ "${sig_rc}" -ne 0 ]] || fail "5c-6 expected non-zero exit after SIGTERM rollback"
assert_file_contains "${TMP_ROOT}/rollback-signal.log" "rolling back to previous working installation"
assert_prev_install_restored_and_running "5c-6 SIGTERM mid-install"
pass "rollback verified across package, ENOSPC, unit-write, start, readiness, and SIGTERM failures"

# Explicit --with-desktop installs x11-utils + xauth and writes new -auth + readiness ExecStartPost
: > "${SANDBOX_STATE}/apt.log"
printf 'ID=debian\nVERSION_ID="12"\nPRETTY_NAME="Debian 12 (Fixture)"\n' > "${SANDBOX}/etc/os-release"
if ! FAKE_CHROMIUM_CANDIDATE="120.0-1" run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop --no-start > "${TMP_ROOT}/bare.log" 2>&1; then
  cat "${TMP_ROOT}/bare.log" >&2
  fail "bare Xvfb install failed"
fi
printf 'ID=ubuntu\nVERSION_ID="24.04"\nPRETTY_NAME="Ubuntu 24.04 LTS (Fixture)"\n' > "${SANDBOX}/etc/os-release"
assert_file_contains "${SANDBOX_STATE}/apt.log" "x11-utils"
assert_file_contains "${SANDBOX_STATE}/apt.log" "xauth"
assert_file_not_contains "${SANDBOX_STATE}/apt.log" "xfce4-terminal"
assert_file_not_contains "${SANDBOX_STATE}/apt.log" "thunar"
assert_file_contains "${SANDBOX}/etc/systemd/system/praxi-xvfb.service" "praxi-wait-x11.sh"
assert_file_contains "${SANDBOX}/etc/systemd/system/praxi-xvfb.service" "Upholds=praxi.service"
[[ ! -f "${SANDBOX}/etc/systemd/system/praxi-desktop.service" ]] || fail "bare Xvfb should not install praxi-desktop.service"
pass "existing bare-Xvfb install preserved on no-flag upgrade and upgraded on --with-desktop"

# 6. Explicit --with-desktop-session install (with default 1920x1080x24 when no desktop unit exists yet)
rm -f "${SANDBOX}/etc/systemd/system/praxi-xvfb.service"
run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
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
XAUTH_EXEC="${SANDBOX}/opt/praxi/libexec/praxi-xauth-init.sh"

for f in "${SLICE_FILE}" "${XVFB_FILE}" "${DESK_FILE}" "${SVC_FILE}" "${NODE_FILE}" "${DROPIN_FILE}" "${ENV_FILE}" "${NODE_ENV}" "${SESSION_EXEC}" "${XAUTH_EXEC}"; do
  [[ -f "${f}" ]] || fail "expected generated file missing: ${f}"
done

assert_file_contains "${SLICE_FILE}" "CPUQuota=100%"
assert_file_contains "${SLICE_FILE}" "MemoryHigh=3G"
assert_file_contains "${SLICE_FILE}" "MemoryMax=4G"
for u in "${XVFB_FILE}" "${DESK_FILE}" "${SVC_FILE}" "${NODE_FILE}"; do
  assert_file_contains "${u}" "Slice=praxi.slice"
  assert_file_contains "${u}" "User=praxi"
done
assert_file_contains "${XVFB_FILE}" "-screen 0 1920x1080x24"
assert_file_contains "${XVFB_FILE}" "-nolisten tcp"
assert_file_contains "${XVFB_FILE}" "-auth ${SANDBOX}/var/lib/praxi/.Xauthority"
assert_file_contains "${XVFB_FILE}" "Upholds=praxi-desktop.service praxi.service"
assert_file_not_contains "${XVFB_FILE}" "-ac"
assert_file_not_contains "${XVFB_FILE}" "-fbdir"
assert_file_not_contains "${XVFB_FILE}" "xhost"

assert_file_contains "${DESK_FILE}" "BindsTo=praxi-xvfb.service"
assert_file_contains "${DESK_FILE}" "PartOf=praxi-xvfb.service"
assert_file_contains "${DESK_FILE}" "JoinsNamespaceOf=praxi-xvfb.service"
assert_file_contains "${DESK_FILE}" "Restart=always"
assert_file_contains "${DESK_FILE}" "StartLimitIntervalSec=60"
assert_file_contains "${DESK_FILE}" "StartLimitBurst=5"
assert_file_contains "${DESK_FILE}" "Environment=PRAXI_ENABLE_SESSION_APPS=0"
assert_file_contains "${DESK_FILE}" "KillMode=control-group"
assert_file_not_contains "${DESK_FILE}" "praxi.env"
awk '/^\[Service\]/{in_svc=1} /^\[/{if(!/^\[Service\]/)in_svc=0} in_svc && /^JoinsNamespaceOf=/{exit 1}' "${DESK_FILE}" || fail "JoinsNamespaceOf must be in [Unit], not [Service]"

assert_file_contains "${DROPIN_FILE}" "BindsTo=praxi-xvfb.service"
assert_file_contains "${DROPIN_FILE}" "Wants=praxi-desktop.service"
assert_file_not_contains "${DROPIN_FILE}" "Requires=praxi-desktop.service"
assert_file_contains "${DROPIN_FILE}" "After=praxi-xvfb.service praxi-desktop.service"
assert_file_contains "${DROPIN_FILE}" "JoinsNamespaceOf=praxi-xvfb.service"

assert_file_contains "${ENV_FILE}" "DISPLAY=:99"
assert_file_contains "${ENV_FILE}" "XAUTHORITY=${SANDBOX}/var/lib/praxi/.Xauthority"
assert_file_contains "${ENV_FILE}" "DBUS_SESSION_BUS_ADDRESS=unix:path=${SANDBOX}/var/lib/praxi/.dbus-session"
if grep -q '^BOT_MAX_COMPUTER_RUNS=' "${ENV_FILE}"; then
  fail "fresh praxi.env should leave BOT_MAX_COMPUTER_RUNS commented for daemon RAM auto-sizing"
fi
if grep -q '^BOT_BROWSER_IDLE_SECONDS=' "${ENV_FILE}"; then
  fail "fresh praxi.env should leave BOT_BROWSER_IDLE_SECONDS commented for daemon default"
fi

assert_file_contains "${NODE_ENV}" "DISPLAY=:99"
assert_file_contains "${NODE_ENV}" "DBUS_SESSION_BUS_ADDRESS=unix:path=${SANDBOX}/var/lib/praxi/.dbus-session"
assert_file_not_contains "${NODE_ENV}" "BOT_SECRETS_PASSPHRASE"

# Verify real file modes & directory ownership/mode logs
assert_file_contains "${TMP_ROOT}/xfce.log" "daemon is stopped (--no-start)"
[[ "$(stat -c '%a' "${ENV_FILE}")" == "640" ]] || fail "expected 0640 on ${ENV_FILE}"
[[ "$(stat -c '%a' "${NODE_ENV}")" == "640" ]] || fail "expected 0640 on ${NODE_ENV}"
assert_file_contains "${SANDBOX_STATE}/perms.log" "chown root:praxi ${ENV_FILE} ${NODE_ENV}"
assert_file_contains "${SANDBOX_STATE}/perms.log" "install -d owner=praxi group=praxi mode=0750 ${SANDBOX}/var/lib/praxi"
assert_file_contains "${SANDBOX_STATE}/perms.log" "install -d owner=root group=praxi mode=0750 ${SANDBOX}/etc/praxi"

assert_file_contains "${SESSION_EXEC}" "xfwm4 --compositor=off"
assert_file_contains "${SESSION_EXEC}" "xfce4-screensaver.desktop"
assert_file_contains "${SESSION_EXEC}" "xfce4-power-manager.desktop"
assert_file_contains "${SESSION_EXEC}" "xfce4-terminal.desktop"
assert_file_contains "${SESSION_EXEC}" "use_compositing"
# shellcheck disable=SC2016
assert_file_contains "${SESSION_EXEC}" 'chmod 0600 "${DBUS_SOCKET}"'
assert_file_contains "${SESSION_EXEC}" "xfdesktop exited"
assert_file_contains "${SESSION_EXEC}" "xfce4-panel exited"
assert_file_not_contains "${SESSION_EXEC}" "openbox"
assert_file_contains "${SANDBOX}/usr/local/bin/praxi" "Slice=praxi.slice"

# Verify praxi-xauth-init.sh reuses existing cookie on subsequent starts and preserves 0600 mode
: > "${SANDBOX_STATE}/xauth.log"
TEST_XAUTH="${TMP_ROOT}/test.Xauthority"
run_timed bash "${XAUTH_EXEC}" :99 "${TEST_XAUTH}"
[[ "$(stat -c '%a' "${TEST_XAUTH}")" == "600" ]] || fail "expected 0600 on ${TEST_XAUTH}"
first_content="$(cat "${TEST_XAUTH}")"
run_timed bash "${XAUTH_EXEC}" :99 "${TEST_XAUTH}"
second_content="$(cat "${TEST_XAUTH}")"
[[ "${first_content}" == "${second_content}" ]] || fail "praxi-xauth-init.sh regenerated cookie instead of reusing existing"
add_calls="$(grep -c ' add :99 ' "${SANDBOX_STATE}/xauth.log" || true)"
[[ "${add_calls}" -eq 1 ]] || fail "expected exactly 1 xauth add call across two starts, got ${add_calls}"
pass "explicit --with-desktop-session generates secure Xfce/Xvfb/slice topology, modes, and xauth reuse"

# 6b. Verify systemd-analyze on generated praxi.slice when available (before --without-desktop removes it)
if command -v systemd-analyze >/dev/null 2>&1; then
  run_timed systemd-analyze verify "${SLICE_FILE}"
  pass "systemd-analyze verify ${SLICE_FILE} clean"
else
  echo "SKIP: systemd-analyze not available on this platform"
fi

# 6c. Behavioral execution of praxi-xfce-session.sh (dbus socket 0600, Hidden=true app overrides, xfdesktop restart, xfwm4 fatal exit)
SESSION_TEST_DIR="${TMP_ROOT}/session-behavior"
SESSION_BIN="${SESSION_TEST_DIR}/bin"
SESSION_HOME="${SESSION_TEST_DIR}/home"
mkdir -p "${SESSION_BIN}" "${SESSION_HOME}"
cat > "${SESSION_BIN}/dbus-daemon" <<'EOF'
#!/usr/bin/env bash
sock=""
for a in "$@"; do
  case "${a}" in
    --address=unix:path=*) sock="${a#--address=unix:path=}" ;;
  esac
done
python3 -c '
import os, socket, sys, time
p = sys.argv[1]
if os.path.exists(p): os.unlink(p)
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(p)
s.listen(1)
time.sleep(20)
' "${sock}"
EOF
cat > "${SESSION_BIN}/xfdesktop" <<EOF
#!/usr/bin/env bash
printf 'start\n' >> "${SESSION_TEST_DIR}/xfdesktop.log"
if [[ ! -f "${SESSION_TEST_DIR}/xfdesktop.once" ]]; then
  touch "${SESSION_TEST_DIR}/xfdesktop.once"
  sleep 0.2
  exit 1
fi
exec sleep 20
EOF
cat > "${SESSION_BIN}/xfce4-panel" <<'EOF'
#!/usr/bin/env bash
exec sleep 20
EOF
for noop_cmd in dbus-update-activation-environment gsettings gdbus xfconf-query xset update-desktop-database; do
  cat > "${SESSION_BIN}/${noop_cmd}" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
done
cat > "${SESSION_BIN}/xfsettingsd" <<'EOF'
#!/usr/bin/env bash
exec sleep 20
EOF
cat > "${SESSION_BIN}/xfwm4" <<EOF
#!/usr/bin/env bash
for _ in \$(seq 1 30); do
  if [[ "\$(wc -l < "${SESSION_TEST_DIR}/xfdesktop.log" 2>/dev/null || echo 0)" -ge 2 ]] && [[ -S "${SESSION_HOME}/.dbus-session" ]]; then
    stat -c '%a' "${SESSION_HOME}/.dbus-session" > "${SESSION_TEST_DIR}/dbus.mode"
    exit 1
  fi
  sleep 0.2
done
exit 0
EOF
chmod +x "${SESSION_BIN}"/*
if PATH="${SESSION_BIN}:${PATH}" HOME="${SESSION_HOME}" \
  DISPLAY=:99 XAUTHORITY="${TEST_XAUTH}" PRAXI_ENABLE_SESSION_APPS=0 \
  XDG_CONFIG_HOME="${SESSION_HOME}/.config" XDG_DATA_HOME="${SESSION_HOME}/.local/share" XDG_CACHE_HOME="${SESSION_HOME}/.cache" \
  run_timed bash "${SESSION_EXEC}" > "${SESSION_TEST_DIR}/run.log" 2>&1; then
  fail "praxi-xfce-session.sh should exit non-zero when xfwm4 dies"
fi
assert_file_contains "${SESSION_TEST_DIR}/run.log" "warning: xfdesktop exited"
assert_file_contains "${SESSION_TEST_DIR}/run.log" "error: xfwm4 exited"
[[ "$(cat "${SESSION_TEST_DIR}/dbus.mode" 2>/dev/null)" == "600" ]] || fail "dbus socket was not 0600"
assert_file_contains "${SESSION_HOME}/.local/share/applications/xfce4-terminal.desktop" "Hidden=true"
assert_file_contains "${SESSION_HOME}/.local/share/applications/thunar.desktop" "Hidden=true"
pass "praxi-xfce-session.sh hides terminal/thunar, sets dbus 0600, restarts xfdesktop, exits on xfwm4 death"

# 6d. --with-session-apps installs terminal/thunar and sets PRAXI_ENABLE_SESSION_APPS=1, --without-session-apps reverts to 0
: > "${SANDBOX_STATE}/apt.log"
printf 'ID=debian\nVERSION_ID="12"\nPRETTY_NAME="Debian 12 (Fixture)"\n' > "${SANDBOX}/etc/os-release"
FAKE_CHROMIUM_CANDIDATE="120.0-1" run_ok "${TMP_ROOT}/session-apps-on.log" bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop-session --with-session-apps --no-start
printf 'ID=ubuntu\nVERSION_ID="24.04"\nPRETTY_NAME="Ubuntu 24.04 LTS (Fixture)"\n' > "${SANDBOX}/etc/os-release"
assert_file_contains "${TMP_ROOT}/session-apps-on.log" "bypassing the daemon shell permission engine"
assert_file_contains "${SANDBOX_STATE}/apt.log" "xfce4-terminal"
assert_file_contains "${SANDBOX_STATE}/apt.log" "thunar"
assert_file_contains "${DESK_FILE}" "Environment=PRAXI_ENABLE_SESSION_APPS=1"
run_ok "${TMP_ROOT}/session-apps-off.log" bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --without-session-apps --skip-packages --no-start
assert_file_contains "${DESK_FILE}" "Environment=PRAXI_ENABLE_SESSION_APPS=0"
pass "--with-session-apps / --without-session-apps toggles terminal/thunar packages and PRAXI_ENABLE_SESSION_APPS"

# 6e. systemd < 249 omits Upholds= and uses ExecStartPost uphold fallback for dependent recovery; >= 249 uses Upholds=
UPHOLD_EXEC="${SANDBOX}/opt/praxi/libexec/praxi-xvfb-uphold.sh"
FAKE_SYSTEMD_VERSION=239 run_ok "${TMP_ROOT}/old-systemd.log" bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop-session --skip-packages --no-start
assert_file_contains "${TMP_ROOT}/old-systemd.log" "omitting Upholds="
assert_file_contains "${TMP_ROOT}/old-systemd.log" "ExecStartPost uphold fallback"
assert_file_not_contains "${XVFB_FILE}" "Upholds="
assert_file_contains "${XVFB_FILE}" "ExecStartPost=!${UPHOLD_EXEC}"
assert_file_contains "${XVFB_FILE}" "Restart=on-failure"
assert_file_contains "${DESK_FILE}" "BindsTo=praxi-xvfb.service"
assert_file_contains "${DESK_FILE}" "Restart=always"
assert_file_contains "${DROPIN_FILE}" "BindsTo=praxi-xvfb.service"
assert_file_contains "${DROPIN_FILE}" "Wants=praxi-desktop.service"
[[ -x "${UPHOLD_EXEC}" ]] || fail "praxi-xvfb-uphold.sh missing or not executable"
printf 'praxi-desktop.service\npraxi.service\n' > "${SANDBOX_STATE}/enabled_units"
: > "${SANDBOX_STATE}/active_units"
: > "${SANDBOX_STATE}/systemctl.log"
FAKE_STATEFUL_SYSTEMCTL=1 run_timed bash "${UPHOLD_EXEC}"
grep -Fxq "praxi-desktop.service" "${SANDBOX_STATE}/active_units" || fail "praxi-xvfb-uphold.sh did not start praxi-desktop.service"
grep -Fxq "praxi.service" "${SANDBOX_STATE}/active_units" || fail "praxi-xvfb-uphold.sh did not start praxi.service"
assert_file_contains "${SANDBOX_STATE}/systemctl.log" "start --no-block praxi-desktop.service"
assert_file_contains "${SANDBOX_STATE}/systemctl.log" "start --no-block praxi.service"
# Upgrade back to systemd >= 249 (both no-flag and --with-desktop-session) removes fallback and emits Upholds=
run_ok "${TMP_ROOT}/new-systemd-noflag.log" bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --skip-packages --no-start
assert_file_contains "${XVFB_FILE}" "Upholds=praxi-desktop.service praxi.service"
assert_file_not_contains "${XVFB_FILE}" "praxi-xvfb-uphold.sh"
run_ok "${TMP_ROOT}/new-systemd.log" bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop-session --skip-packages --no-start
assert_file_contains "${XVFB_FILE}" "Upholds=praxi-desktop.service praxi.service"
assert_file_not_contains "${XVFB_FILE}" "praxi-xvfb-uphold.sh"
pass "systemd < 249 uses ExecStartPost uphold fallback while >= 249 emits Upholds="

# 7. Re-run idempotence preserves desktop session + an EXISTING Xvfb geometry (both no-flag and explicit --with-desktop-session).
# The fresh default is 1920x1080x24 (asserted in section 6), so seed a different geometry here: asserting
# the default would also pass when preservation breaks.
[[ -f "${DESK_FILE}" ]] || fail "idempotent re-run precondition: praxi-desktop.service missing"
sed 's/-screen 0 1920x1080x24/-screen 0 1280x800x24/' "${XVFB_FILE}" > "${XVFB_FILE}.seed"
mv -f "${XVFB_FILE}.seed" "${XVFB_FILE}"
assert_file_contains "${XVFB_FILE}" "-screen 0 1280x800x24"
run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz"   --skip-packages --no-start > "${TMP_ROOT}/idempotent.log" 2>&1
[[ -f "${DESK_FILE}" ]] || fail "idempotent re-run lost praxi-desktop.service"
assert_file_contains "${XVFB_FILE}" "-screen 0 1280x800x24"
assert_file_not_contains "${XVFB_FILE}" "1920x1080x24"

run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz"   --with-desktop-session --skip-packages --no-start > "${TMP_ROOT}/idempotent-flag.log" 2>&1
assert_file_contains "${XVFB_FILE}" "-screen 0 1280x800x24"
assert_file_not_contains "${XVFB_FILE}" "1920x1080x24"
pass "idempotent upgrade (no-flag and --with-desktop-session) preserves praxi-desktop.service and an existing 1280x800x24 geometry"

# 8. Explicit --without-desktop-session removes desktop session but keeps Xvfb
run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --without-desktop-session --skip-packages --no-start > "${TMP_ROOT}/downgrade.log" 2>&1
[[ ! -f "${DESK_FILE}" ]] || fail "--without-desktop-session did not remove praxi-desktop.service"
[[ -f "${XVFB_FILE}" ]] || fail "--without-desktop-session should keep praxi-xvfb.service"
assert_file_not_contains "${ENV_FILE}" "DBUS_SESSION_BUS_ADDRESS"
assert_file_contains "${ENV_FILE}" "DISPLAY=:99"
pass "--without-desktop-session rollback removes Xfce session cleanly"

# 9. setup-headless.sh reconciliation, root-in-user-mode rejection, & package list checks
# 9a. Root running default --user mode without --system is rejected
if FAKE_UID=0 HOME="${SANDBOX}/home/tester" run_timed bash "${SB_SETUP}" --display :98 --skip-packages --no-start > "${TMP_ROOT}/root-user.log" 2>&1; then
  fail "setup-headless without --system should fail when run as root"
fi
assert_file_contains "${TMP_ROOT}/root-user.log" "pass --system or run as an unprivileged user"
pass "setup-headless rejects root in default --user mode"

# 9b. User mode refuses :99 when praxi-xvfb.service owns :99
if FAKE_UID=1000 HOME="${SANDBOX}/home/tester" run_timed bash "${SB_SETUP}" --user --display :99 --skip-packages --no-start > "${TMP_ROOT}/conflict-disp.log" 2>&1; then
  fail "setup-headless --user should refuse display :99 owned by praxi-xvfb.service"
fi
assert_file_contains "${TMP_ROOT}/conflict-disp.log" "owned by system unit praxi-xvfb.service"
pass "setup-headless --user prevents duplicate display server on :99"

# 9c. User mode on :98 creates minimal Xfce session with private Xauthority (no openbox, no -ac)
FAKE_UID=1000 HOME="${SANDBOX}/home/tester" run_timed bash "${SB_SETUP}" --user --display :98 --skip-packages --no-start > "${TMP_ROOT}/user-xfce.log" 2>&1
USER_SESSION="${SANDBOX}/home/tester/.local/bin/praxi-headless-session.sh"
USER_UNIT="${SANDBOX}/home/tester/.config/systemd/user/praxi-headless-desktop.service"
USER_WANTS="${SANDBOX}/home/tester/.config/systemd/user/default.target.wants/praxi-headless-desktop.service"
mkdir -p "$(dirname "${USER_WANTS}")"
ln -sf "${USER_UNIT}" "${USER_WANTS}"
[[ -f "${USER_SESSION}" && -f "${USER_UNIT}" ]] || fail "user-mode Xfce files missing"
assert_file_contains "${USER_SESSION}" "-nolisten tcp"
assert_file_contains "${USER_SESSION}" "-auth"
assert_file_not_contains "${USER_SESSION}" " -ac "
assert_file_not_contains "${USER_UNIT}" "openbox"
pass "setup-headless --user creates private Xfce session on :98"

# 9d. System mode on :99 preserves :98 user unit & wants symlink, then removes :99 user unit via disable --now
: > "${SANDBOX_STATE}/apt.log"
: > "${SANDBOX_STATE}/systemctl.log"
FAKE_ACTIVE_UNITS="praxi-headless-desktop.service" FAKE_UID=0 HOME="${SANDBOX}/home/tester" \
  run_timed bash "${SB_SETUP}" --system --display :99 --screen 1280x800x24 --no-start > "${TMP_ROOT}/sys-setup-preserve98.log" 2>&1
[[ -f "${USER_UNIT}" && -L "${USER_WANTS}" && -e "${USER_WANTS}" ]] || fail "setup-headless --system on :99 should preserve :98 user unit and wants symlink"
assert_file_not_contains "${SANDBOX_STATE}/systemctl.log" "disable --now praxi-headless-desktop.service"

# 9e. A.6: user unit referencing an ExecStart= wrapper script containing DISPLAY=":99" (even without DISPLAY in unit file)
cat > "${USER_SESSION}" <<'EOF'
#!/usr/bin/env bash
export DISPLAY=":99"
EOF
cat > "${USER_UNIT}" <<EOF
[Service]
ExecStart=${USER_SESSION}
EOF
: > "${SANDBOX_STATE}/systemctl.log"
FAKE_ACTIVE_UNITS="praxi-headless-desktop.service" FAKE_UID=0 HOME="${SANDBOX}/home/tester" \
  run_timed bash "${SB_SETUP}" --system --display :99 --screen 1280x800x24 --skip-packages --no-start > "${TMP_ROOT}/sys-setup.log" 2>&1
[[ -f "${DESK_FILE}" ]] || fail "setup-headless --system did not create praxi-desktop.service"
[[ ! -f "${USER_UNIT}" && ! -L "${USER_WANTS}" && ! -e "${USER_WANTS}" ]] || fail "setup-headless --system did not remove conflicting :99 user unit (ExecStart script) and wants symlink"
assert_file_contains "${SANDBOX_STATE}/systemctl.log" "--user disable --now praxi-headless-desktop.service"
assert_file_contains "${SANDBOX_STATE}/apt.log" "xfwm4 xfce4-panel xfdesktop4"
assert_file_not_contains "${SANDBOX_STATE}/apt.log" "xfce4-terminal"
assert_file_not_contains "${SANDBOX_STATE}/apt.log" "thunar"
assert_file_not_contains "${SANDBOX_STATE}/apt.log" "chromium-browser"
assert_file_not_contains "${SANDBOX_STATE}/apt.log" "x11vnc"
assert_file_not_contains "${SANDBOX_STATE}/apt.log" "cups"

# Also verify install-linux.sh cleans conflicting :99 user unit (ExecStart script) while preserving :98
cat > "${USER_UNIT}" <<EOF
[Service]
ExecStart=${USER_SESSION}
EOF
ln -sf "${USER_UNIT}" "${USER_WANTS}"
: > "${SANDBOX_STATE}/systemctl.log"
FAKE_ACTIVE_UNITS="praxi-headless-desktop.service" HOME="${SANDBOX}/home/tester" \
  run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop-session --skip-packages --no-start > "${TMP_ROOT}/install-clean-user.log" 2>&1
[[ ! -f "${USER_UNIT}" && ! -L "${USER_WANTS}" && ! -e "${USER_WANTS}" ]] || fail "install-linux.sh did not remove conflicting :99 user unit"
assert_file_contains "${SANDBOX_STATE}/systemctl.log" "--user disable --now praxi-headless-desktop.service"
pass "setup-headless --system and install-linux reconcile :99 (including ExecStart scripts) without touching :98"

# 9f. Parity checks between install-linux.sh and setup-headless.sh across desktop, headless-only, and --user layouts
PARITY_INST="${TMP_ROOT}/parity-install"
mkdir -p "${PARITY_INST}"
cp -f "${SLICE_FILE}" "${XVFB_FILE}" "${DESK_FILE}" "${DROPIN_FILE}" \
  "${SESSION_EXEC}" "${XAUTH_EXEC}" "${UPHOLD_EXEC}" \
  "${SANDBOX}/opt/praxi/libexec/praxi-wait-x11.sh" \
  "${SANDBOX}/opt/praxi/libexec/praxi-wait-desktop.sh" "${PARITY_INST}/"
FAKE_UID=0 run_timed bash "${SB_SETUP}" --system --display :99 --screen 1280x800x24 --skip-packages --no-start > "${TMP_ROOT}/parity-setup.log" 2>&1
for rel in praxi.slice praxi-xvfb.service praxi-desktop.service desktop.conf \
  praxi-xfce-session.sh praxi-xauth-init.sh praxi-wait-x11.sh praxi-wait-desktop.sh praxi-xvfb-uphold.sh; do
  case "${rel}" in
    desktop.conf) rhs="${DROPIN_FILE}" ;;
    praxi-xfce-session.sh|praxi-xauth-init.sh|praxi-wait-x11.sh|praxi-wait-desktop.sh|praxi-xvfb-uphold.sh) rhs="${SANDBOX}/opt/praxi/libexec/${rel}" ;;
    *) rhs="${SANDBOX}/etc/systemd/system/${rel}" ;;
  esac
  diff -u "${PARITY_INST}/${rel}" "${rhs}" || fail "template drift between install-linux.sh and setup-headless.sh (desktop) in ${rel}"
done

# Headless-only (Xvfb without Xfce session) parity
PARITY_HEADLESS="${TMP_ROOT}/parity-headless"
mkdir -p "${PARITY_HEADLESS}"
run_ok "${TMP_ROOT}/parity-headless-inst.log" bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --with-desktop --without-desktop-session --display :99 --screen 1920x1080x24 --skip-packages --no-start
[[ ! -f "${DESK_FILE}" ]] || fail "headless-only install should not have praxi-desktop.service"
cp -f "${SLICE_FILE}" "${XVFB_FILE}" "${DROPIN_FILE}" "${XAUTH_EXEC}" "${UPHOLD_EXEC}" \
  "${SANDBOX}/opt/praxi/libexec/praxi-wait-x11.sh" "${PARITY_HEADLESS}/"
FAKE_UID=0 run_ok "${TMP_ROOT}/parity-headless-setup.log" bash "${SB_SETUP}" --system --session none \
  --display :99 --screen 1920x1080x24 --skip-packages --no-start
[[ ! -f "${DESK_FILE}" ]] || fail "headless-only setup-headless should not have praxi-desktop.service"
for rel in praxi.slice praxi-xvfb.service desktop.conf praxi-xauth-init.sh praxi-wait-x11.sh praxi-xvfb-uphold.sh; do
  case "${rel}" in
    desktop.conf) rhs="${DROPIN_FILE}" ;;
    praxi-xauth-init.sh|praxi-wait-x11.sh|praxi-xvfb-uphold.sh) rhs="${SANDBOX}/opt/praxi/libexec/${rel}" ;;
    *) rhs="${SANDBOX}/etc/systemd/system/${rel}" ;;
  esac
  diff -u "${PARITY_HEADLESS}/${rel}" "${rhs}" || fail "template drift between install-linux.sh and setup-headless.sh (headless-only) in ${rel}"
done

# --user mode shared helper script parity
FAKE_UID=1000 HOME="${SANDBOX}/home/tester" run_ok "${TMP_ROOT}/parity-user-setup.log" bash "${SB_SETUP}" --user \
  --display :98 --screen 1280x800x24 --skip-packages --no-start
diff -u "${PARITY_INST}/praxi-xauth-init.sh" "${SANDBOX}/home/tester/.local/bin/praxi-xauth-init.sh" || fail "helper drift in --user praxi-xauth-init.sh"
diff -u "${PARITY_INST}/praxi-xfce-session.sh" "${SANDBOX}/home/tester/.local/bin/praxi-xfce-session.sh" || fail "helper drift in --user praxi-xfce-session.sh"
pass "install-linux.sh and setup-headless.sh generate identical units/helpers across desktop, headless-only, and --user layouts"

# 10. Explicit --without-desktop removes units, praxi.slice, Slice= lines, DISPLAY, and XAUTHORITY (Nit 9)
run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --without-desktop --skip-packages --no-start > "${TMP_ROOT}/nodesk.log" 2>&1
[[ ! -f "${DESK_FILE}" && ! -f "${XVFB_FILE}" && ! -f "${SLICE_FILE}" ]] || fail "--without-desktop did not remove desktop units and praxi.slice"
assert_file_not_contains "${SVC_FILE}" "Slice=praxi.slice"
assert_file_not_contains "${NODE_FILE}" "Slice=praxi.slice"
assert_file_not_contains "${ENV_FILE}" "DISPLAY="
assert_file_not_contains "${ENV_FILE}" "XAUTHORITY="
pass "--without-desktop removes Xvfb, desktop session, praxi.slice, and Slice= cleanly"

# 10b. Always-on host polish: restart limits, log cap, dry-run, --update, uninstall cleanup
run_timed bash "${SB_INSTALL}" --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --headless --skip-packages --no-start > "${TMP_ROOT}/headless.log" 2>&1
assert_file_contains "${SVC_FILE}" "Restart=on-failure"
assert_file_contains "${SVC_FILE}" "StartLimitBurst=10"
assert_file_contains "${SVC_FILE}" "NoNewPrivileges=true"
assert_file_contains "${SVC_FILE}" "/bot serve --host"
JOURNALD_DIR_SB="${SANDBOX}/etc/systemd/journald.conf.d"
JOURNALD_FILE="${JOURNALD_DIR_SB}/50-praxi.conf"
# The log limit is per unit; the installer must not write host-wide journald
# config or restart journald (shared servers).
assert_file_contains "${SVC_FILE}" "LogRateLimitBurst=2000"
[[ ! -e "${JOURNALD_FILE}" ]] || fail "installer wrote a host-wide journald drop-in"
assert_file_not_contains "${SANDBOX_STATE}/systemctl.log" "journald"
# Legacy file from an older installer: ours is removed, an admin's own is kept.
mkdir -p "${JOURNALD_DIR_SB}"
printf '# Written by the Praxi installer; removed by install-linux.sh --uninstall.\n[Journal]\nSystemMaxUse=500M\n' > "${JOURNALD_FILE}"
: > "${SANDBOX_STATE}/systemctl.log"
run_timed bash "${SB_INSTALL}" --update --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --skip-packages --no-start > "${TMP_ROOT}/update-legacy.log" 2>&1
[[ ! -e "${JOURNALD_FILE}" ]] || fail "--update left the legacy journald drop-in"
assert_file_not_contains "${SANDBOX_STATE}/systemctl.log" "journald"
printf '[Journal]\nSystemMaxUse=2G\n' > "${JOURNALD_FILE}"
assert_file_contains "${XVFB_FILE}" "-screen 0 1920x1080x24"
run_timed bash "${SB_INSTALL}" --dry-run --headless --screen 1280x720x24 > "${TMP_ROOT}/dry-install.log" 2>&1
assert_file_contains "${TMP_ROOT}/dry-install.log" "Xvfb :99 screen 1280x720x24"
assert_file_contains "${TMP_ROOT}/dry-install.log" "nothing was changed"
if run_timed bash "${SB_INSTALL}" --update --uninstall > "${TMP_ROOT}/update-uninstall.log" 2>&1; then
  fail "--update with --uninstall should fail"
fi
run_timed bash "${SB_INSTALL}" --update --version v0.1.0 --file "${RELEASE_DIR}/praxi-linux-x86_64.tar.gz" \
  --skip-packages --no-start > "${TMP_ROOT}/update.log" 2>&1
assert_file_contains "${TMP_ROOT}/update.log" "updating in place"
run_timed bash "${SB_INSTALL}" --uninstall > "${TMP_ROOT}/uninstall.log" 2>&1
[[ -f "${JOURNALD_FILE}" ]] || fail "--uninstall removed a journald drop-in it did not write"
assert_file_contains "${JOURNALD_FILE}" "SystemMaxUse=2G"
assert_file_not_contains "${SANDBOX_STATE}/systemctl.log" "journald"
rm -f "${JOURNALD_FILE}"
pass "host polish: restart limits, per-unit log limit, no journald changes, dry-run, --update, uninstall cleanup"

# 11. systemd-analyze verify check on remaining service unit when available
if command -v systemd-analyze >/dev/null 2>&1; then
  echo "INFO: systemd-analyze found; checking remaining service unit syntax"
  run_timed systemd-analyze verify "${SVC_FILE}" 2>&1 || true
else
  echo "SKIP: systemd-analyze not available on this platform (run on Linux target VM)"
fi

# 12. The systemd manager reports the replacement command from the loaded drop-in.
SB_PROVISION="${TMP_ROOT}/provision.sh"
make_sandboxed_script "${REPO_ROOT}/crates/host-provision/scripts/provision.sh" "${SB_PROVISION}"
FAKE_PRAXI_EXEC_START='{ path=/opt/praxi/bin/bot ; argv[]=/opt/praxi/bin/bot serve --peer --host ; ignore_errors=no ; pid=123 }' \
  run_ok "${TMP_ROOT}/provision-host-mode.log" bash "${SB_PROVISION}" status
assert_file_contains "${TMP_ROOT}/provision-host-mode.log" "unit_hostmode=1"
FAKE_PRAXI_EXEC_START='{ path=/opt/praxi/bin/bot ; argv[]=/opt/praxi/bin/bot serve --peer ; ignore_errors=no ; pid=123 }' \
  run_ok "${TMP_ROOT}/provision-standalone-mode.log" bash "${SB_PROVISION}" status
assert_file_contains "${TMP_ROOT}/provision-standalone-mode.log" "unit_hostmode=0"
pass "provisioning detects host mode from the effective systemd command"

echo "ALL PACKAGING HERMETIC TESTS PASSED"
