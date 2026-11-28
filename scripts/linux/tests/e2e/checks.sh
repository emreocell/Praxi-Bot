#!/usr/bin/env bash
# Runs INSIDE the fresh Ubuntu 24.04 systemd container as root; started by run.sh, which also
# prepares /e2e (the installer, release-shaped tarballs, e2e-gh.sh). Usage: checks.sh host-mode|headless
#
# Order: refusals (nothing may change) -> install -> services -> API with the token -> praxi wrapper ->
# Node.js/npm for the service user -> browser -> --update -> newer tag -> --uninstall -> --purge.
# A failed check is reported and the run goes on, except where the next step cannot make sense.

# `cond && pass ... || fail ...` is safe here (pass never fails), and `bash -c '...'` checks keep their
# single quotes on purpose (the inner shell expands them).
# shellcheck disable=SC2015,SC2016
set -uo pipefail

MODE="${1:-host-mode}"
TAG_V1="${E2E_TAG_V1:?}"
TAG_V2="${E2E_TAG_V2:?}"
ARCH="${E2E_ARCH:?}"
ASSET="praxi-linux-${ARCH}.tar.gz"
INSTALLER=/e2e/install-linux.sh
PORT=47831 # not the default, so "kept on update" and "--port" are both exercised
DATA=/var/lib/praxi
LOG=/tmp/e2e-installer.log

case "${MODE}" in
  host-mode) MODE_FLAGS=(--host-mode) ;;
  headless) MODE_FLAGS=(--headless) ;;
  *) echo "unknown mode ${MODE}" >&2; exit 2 ;;
esac

# Chrome's sandbox needs unprivileged user namespaces. Ubuntu 24.04 restricts them by default and lets
# named AppArmor profiles through; the profile the installer loads on a real server cannot be loaded
# from inside a container. CI therefore switches the restriction off on its runner and sets
# E2E_REQUIRE_BROWSER=1; anywhere else the checks that need a STARTED browser are skipped, loudly.
USERNS_BLOCKED=0
if [[ "${E2E_REQUIRE_BROWSER:-0}" != 1 && "$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns 2>/dev/null || echo 0)" == 1 ]]; then
  USERNS_BLOCKED=1
fi
BROWSER_INSTALLED=0

FAILS=0
section() { printf '\n-- %s\n' "$*"; }
skip() { printf '  skip  %s\n' "$*"; }
pass() { printf '  ok    %s\n' "$*"; }
fail() { printf '  FAIL  %s\n' "$*" >&2; FAILS=$((FAILS + 1)); }
abort() { printf '  ABORT %s\n' "$*" >&2; printf '\n%d check(s) failed before the abort\n' "${FAILS}" >&2; exit 1; }
check() { # <description> <command...>: passes when the command exits 0 (its output is dropped)
  local desc="$1"
  shift
  if "$@" >/dev/null 2>&1; then pass "${desc}"; else fail "${desc}"; fi
}
check_not() { # <description> <command...>: passes when the command exits non-zero
  local desc="$1"
  shift
  if "$@" >/dev/null 2>&1; then fail "${desc}"; else pass "${desc}"; fi
}
installer_log_has() { grep -Fq -- "$1" "${LOG}"; }
run_installer() { # <args...>: output to ${LOG} (and shown on failure by the caller)
  bash "${INSTALLER}" "$@" >"${LOG}" 2>&1
}
show_log() { sed 's/^/    | /' "${LOG}" >&2; }

api_code() { # <path> [curl args...]
  local path="$1"
  shift
  curl -s -o /dev/null -w '%{http_code}' --noproxy '*' --max-time 10 "$@" "http://127.0.0.1:${PORT}${path}"
}
api() { curl -fsS --noproxy '*' --max-time 10 -H "Authorization: Bearer $(cat "${DATA}/api-token")" "http://127.0.0.1:${PORT}$1"; }
startup_note() { # how long the newest daemon start took to listen, for the log (not a check)
  local ms
  ms="$(journalctl -u praxi.service -o cat --no-pager 2>/dev/null | sed -n 's/.*"elapsed_ms":"\([0-9]*\)","phase":"listening".*/\1/p' | tail -n 1)"
  printf '  info  daemon started listening after %s ms\n' "${ms:-?}"
}
wait_healthy() {
  for _ in $(seq 1 40); do
    if curl -fsS --noproxy '*' --max-time 2 "http://127.0.0.1:${PORT}/v1/health" 2>/dev/null | grep -Eq '"status"[[:space:]]*:[[:space:]]*"(ok|setup_required)"'; then return 0; fi
    sleep 1
  done
  return 1
}
doctor_row() { # <row title> <doctor json>: the row's status word
  jq -r --arg n "$1" '[.checks[] | select(.name == $n)][0].status // "missing"' <<<"$2"
}
service_env() { # what the running daemon was started with (PATH, NPM_CONFIG_*), as KEY=VALUE words
  local pid
  pid="$(systemctl show praxi.service -p MainPID --value)"
  tr '\0' '\n' <"/proc/${pid}/environ" | grep -E '^(PATH|NPM_CONFIG_PREFIX|NPM_CONFIG_GLOBALCONFIG)='
}
as_praxi() { # <command...>: as the service user with the daemon's PATH and npm settings
  local -a env_words
  mapfile -t env_words < <(service_env)
  runuser -u praxi -- env -i HOME="${DATA}" "${env_words[@]}" "$@"
}

# ================================================================ fresh machine
section "fresh Ubuntu 24.04 with systemd"
# shellcheck source=/dev/null
VERSION_ID="$(. /etc/os-release && echo "${VERSION_ID}")"
[[ "${VERSION_ID}" == 24.04 ]] && pass "Ubuntu ${VERSION_ID}" || fail "expected Ubuntu 24.04, got ${VERSION_ID}"
[[ "$(ps -o comm= -p 1)" == systemd ]] && pass "systemd is PID 1" || abort "systemd is not PID 1"
check_not "no praxi user yet" id praxi
check_not "no /opt/praxi yet" test -e /opt/praxi
check_not "no gh, curl or node on the machine" bash -c 'command -v gh || command -v curl || command -v node'

# ============================================================ refusals first
section "the installer refuses what it cannot verify (and changes nothing)"
untouched() { ! id praxi >/dev/null 2>&1 && [[ ! -e /opt/praxi && ! -e /etc/systemd/system/praxi.service && ! -e /etc/praxi && ! -e ${DATA} ]]; }

if run_installer --version "${TAG_V1}" --file "/e2e/rel/v1/${ASSET}" "${MODE_FLAGS[@]}"; then
  fail "installed without the trusted /usr/bin/gh"
else
  installer_log_has "the GitHub CLI is required at /usr/bin/gh" && pass "no /usr/bin/gh: refused with the reason" \
    || { fail "no /usr/bin/gh: wrong or missing error"; show_log; }
fi
untouched && pass "nothing was installed" || fail "the refused run left files behind"

# The verifier is a test double. It is root-owned in root-owned directories, like the real package.
install -m 0755 -o root -g root /e2e/e2e-gh.sh /usr/bin/gh
if run_installer --version "${TAG_V1}" --file "/e2e/rel/tampered/${ASSET}" "${MODE_FLAGS[@]}"; then
  fail "installed an archive the verifier rejects"
else
  installer_log_has "provenance verification failed; refusing to install" && pass "tampered archive with a matching .sha256: refused" \
    || { fail "tampered archive: wrong or missing error"; show_log; }
fi
untouched && pass "nothing was installed" || fail "the refused run left files behind"

if run_installer --version "${TAG_V2}" --file "/e2e/rel/v1/${ASSET}" "${MODE_FLAGS[@]}"; then
  fail "installed an archive attested for another tag"
else
  installer_log_has "provenance verification failed; refusing to install" && pass "archive attested for another tag: refused" \
    || { fail "wrong-tag archive: wrong or missing error"; show_log; }
fi
untouched && pass "nothing was installed" || fail "the refused run left files behind"

# ================================================================== install
section "install from the local tarball (${MODE})"
start=$SECONDS
if run_installer --version "${TAG_V1}" --file "/e2e/rel/v1/${ASSET}" --port "${PORT}" "${MODE_FLAGS[@]}"; then
  pass "installer exited 0 in $((SECONDS - start))s"
else
  show_log
  abort "the installer failed"
fi
installer_log_has "verified release provenance for ${TAG_V1}" && pass "provenance verified for ${TAG_V1}" || fail "no provenance line in the installer output"
installer_log_has "checksum OK" && pass "sibling .sha256 checked" || fail "no checksum line in the installer output"
installer_log_has "daemon ready on loopback" && pass "installer's own readiness wait passed" || fail "installer did not report readiness"
grep -Fq "${TAG_V1}" /opt/praxi/VERSION && pass "/opt/praxi/VERSION names ${TAG_V1}" || fail "unexpected /opt/praxi/VERSION: $(cat /opt/praxi/VERSION 2>&1)"

# ================================================================== services
section "services"
check "praxi.service is active" systemctl is-active --quiet praxi.service
check "praxi.service is enabled" systemctl is-enabled --quiet praxi.service
check "praxi.service runs as praxi" bash -c '[[ "$(ps -o user= -p "$(systemctl show praxi.service -p MainPID --value)")" == praxi ]]'
check_not "praxi-node.service is not enabled (nothing to pair yet)" systemctl is-enabled --quiet praxi-node.service
check_not "praxi-node.service is not running" systemctl is-active --quiet praxi-node.service
if [[ "${MODE}" == headless ]]; then
  check "praxi-xvfb.service is active" systemctl is-active --quiet praxi-xvfb.service
  check "Chrome for Testing is installed" /opt/praxi-browser/chrome --version
fi
check "praxi-update.socket is active (host mode)" systemctl is-active --quiet praxi-update.socket
check "praxi-update-check.timer is active (host mode)" systemctl is-active --quiet praxi-update-check.timer
check "unit runs 'bot serve --host'" grep -Eq '^ExecStart=/opt/praxi/bin/bot serve --host$' /etc/systemd/system/praxi.service
check "no failed praxi unit" bash -c '[[ -z "$(systemctl list-units --state=failed --plain --no-legend "praxi*")" ]]'

# ================================================== files, users, permissions
section "users, files and permissions"
check "system user praxi, no login shell" bash -c '[[ "$(getent passwd praxi | cut -d: -f7)" == /usr/sbin/nologin ]]'
check "${DATA} is 0750 praxi:praxi" bash -c '[[ "$(stat -c "%a %U:%G" '"${DATA}"')" == "750 praxi:praxi" ]]'
check "/etc/praxi/praxi.env is 0640 root:praxi" bash -c '[[ "$(stat -c "%a %U:%G" /etc/praxi/praxi.env)" == "640 root:praxi" ]]'
check "praxi-node.env has no secret-store passphrase" bash -c '! grep -q PASSPHRASE /etc/praxi/praxi-node.env'
check "BOT_PORT=${PORT} in praxi.env" grep -Fxq "BOT_PORT=${PORT}" /etc/praxi/praxi.env
check "api-token is 64 hex characters, 0600 praxi" bash -c '[[ "$(cat '"${DATA}"'/api-token)" =~ ^[0-9a-f]{64}$ && "$(stat -c "%a %U" '"${DATA}"'/api-token)" == "600 praxi" ]]'
check "/usr/local/bin/praxi wrapper exists" test -x /usr/local/bin/praxi

# ====================================================== daemon on loopback
section "daemon on 127.0.0.1:${PORT}"
wait_healthy && pass "GET /v1/health answers" || fail "no health answer"
startup_note
health="$(curl -fsS --noproxy '*' "http://127.0.0.1:${PORT}/v1/health" 2>/dev/null || true)"
[[ "${health}" == *'"version"'* ]] && pass "health reports a version" || fail "health: ${health}"
listeners="$(ss -H -ltn "sport = :${PORT}")"
[[ -n "${listeners}" ]] && ! awk '{print $4}' <<<"${listeners}" | grep -qv '^127\.0\.0\.1:' \
  && pass "listening on 127.0.0.1 only" || fail "listeners: ${listeners:-none}"
[[ "$(api_code /v1/doctor)" == 401 ]] && pass "GET /v1/doctor without a token: 401" || fail "no token: HTTP $(api_code /v1/doctor)"
[[ "$(api_code /v1/doctor -H "Authorization: Bearer $(printf '0%.0s' {1..64})")" == 401 ]] && pass "wrong token: 401" || fail "wrong token was not refused"
[[ "$(api_code /v1/doctor -H "Authorization: Bearer $(cat "${DATA}/api-token")")" == 200 ]] && pass "right token: 200" || fail "right token: HTTP $(api_code /v1/doctor -H "Authorization: Bearer $(cat "${DATA}/api-token")")"

# ===================================================== the praxi wrapper
section "praxi command runs as the praxi user"
check "wrapper switches to the service user" grep -Fq 'runuser -u praxi' /usr/local/bin/praxi
# `bot status` talks to the daemon with the token file, which only the praxi user can read; it also
# insists the daemon belongs to the same user. So this succeeds only when the wrapper really is praxi.
check "'praxi status' as root" praxi status
check "'sudo praxi status' as an ordinary user" sudo -u ubuntu sudo praxi status
check_not "ordinary user cannot read the token directly" sudo -u ubuntu cat "${DATA}/api-token"

# ============================================================ praxi doctor
section "praxi doctor"
doctor="$(praxi doctor --json 2>/tmp/e2e-doctor.err)"
if ! jq -e . >/dev/null 2>&1 <<<"${doctor}"; then
  cat /tmp/e2e-doctor.err >&2
  fail "praxi doctor --json did not print JSON: ${doctor}"
  doctor='{"ok":false,"checks":[]}'
fi
jq -r '.checks[] | "    | \(.status)\t\(.name)"' <<<"${doctor}"
# A fresh install has no AI provider yet: that row fails until `praxi setup`. Nothing else may fail.
failing="$(jq -r '[.checks[] | select(.status == "fail") | .name] | sort | join(",")' <<<"${doctor}")"
expected_failing=providers
[[ "${USERNS_BLOCKED}" -eq 0 || "${MODE}" != headless ]] || expected_failing=browser,providers
if [[ "${failing}" == "${expected_failing}" ]]; then
  pass "the only failing row is '${expected_failing}' (no provider set up yet)"
else
  fail "failing rows: '${failing}'"
  jq -r '.checks[] | select(.status == "fail") | "    | \(.name): \(.detail)"' <<<"${doctor}" >&2
fi
for row in "host ownership" "data dir" "secret backend" "model catalog" "daemon" "safe mode" "disk space" "clock" "node / npm"; do
  status="$(doctor_row "${row}" "${doctor}")"
  [[ "${status}" == ok ]] && pass "row '${row}' is ok" || fail "row '${row}' is ${status}"
done

# ============================================================ Node.js / npm
section "Node.js and npm for the service user (no root)"
want_node="v$(sed -n 's/^readonly NODE_VERSION="\(.*\)"$/\1/p' "${INSTALLER}")"
[[ "$(as_praxi node --version)" == "${want_node}" ]] && pass "node ${want_node} (the installer's pin) on the service PATH" || fail "node: $(as_praxi node --version 2>&1)"
check "npm runs" as_praxi npm --version
[[ "$(as_praxi npm config get registry)" == https://registry.npmjs.org/ ]] && pass "npm registry is pinned" || fail "registry: $(as_praxi npm config get registry 2>&1)"
[[ "$(as_praxi npm config get prefix)" == "${DATA}/.npm-global" ]] && pass "global prefix is in the data directory" || fail "prefix: $(as_praxi npm config get prefix 2>&1)"
if as_praxi npm install -g semver@7.6.3 >/tmp/e2e-npm.log 2>&1; then
  pass "npm install -g works as praxi, without root"
else
  sed 's/^/    | /' /tmp/e2e-npm.log >&2
  fail "npm install -g failed"
fi
[[ "$(as_praxi semver 1.2.3 2>&1)" == 1.2.3 ]] && pass "the installed command runs from the service PATH" || fail "semver did not run"
check "the package belongs to praxi" bash -c '[[ "$(stat -c %U '"${DATA}"'/.npm-global/bin/semver)" == praxi ]]'
check "node lives outside the release tree (/opt/praxi-nodejs)" test -x /opt/praxi-nodejs/bin/node

# ================================================================= browser
section "browser"
sandbox_blocked() { [[ "${USERNS_BLOCKED}" -eq 1 ]]; }
if [[ "${MODE}" == host-mode ]]; then
  [[ "$(doctor_row browser "${doctor}")" != ok ]] && pass "no browser before the repair (row: $(doctor_row browser "${doctor}"))" || fail "browser row is already ok on a fresh host"
  # A bare server image may lack the libraries Chrome links against; the error then names the apt
  # packages to install (the same hint the app shows). Follow it once, like the owner would.
  if ! praxi browser install >/tmp/e2e-browser.log 2>&1 && grep -q 'missing system libraries' /tmp/e2e-browser.log; then
    libs="$(grep -o 'apt-get install -y [a-z0-9. -]*' /tmp/e2e-browser.log | head -n 1 | sed 's/^apt-get install -y //')"
    [[ -n "${libs}" ]] && pass "the error names the libraries to install: ${libs}" || fail "the missing-libraries error names no packages"
    # shellcheck disable=SC2086 # the hint is a list of package names
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ${libs} >/tmp/e2e-apt.log 2>&1 || { tail -5 /tmp/e2e-apt.log >&2; fail "apt-get install of the hinted libraries failed"; }
    praxi browser install >/tmp/e2e-browser.log 2>&1
  fi
  if grep -Eq 'Browser installed|already installed' /tmp/e2e-browser.log; then
    pass "'praxi browser install' (as praxi, no root) finished"
    BROWSER_INSTALLED=1
    check "the managed browser is in the data directory" test -d "${DATA}/browser"
    after="$(praxi doctor --json 2>/dev/null || true)"
    [[ "$(doctor_row browser "${after}")" == ok ]] && pass "doctor: browser row is ok after the repair" \
      || fail "doctor: browser row is $(doctor_row browser "${after}"): $(jq -r '[.checks[] | select(.name == "browser")][0].detail' <<<"${after}")"
  elif sandbox_blocked && grep -q 'sandbox cannot start' /tmp/e2e-browser.log; then
    skip "browser start check: this machine restricts user namespaces (see USERNS_BLOCKED); download and checksum were not the problem"
  else
    sed 's/^/    | /' /tmp/e2e-browser.log >&2
    fail "'praxi browser install' failed"
  fi
elif sandbox_blocked; then
  skip "doctor: browser row (the machine restricts user namespaces, so Chrome's sandbox cannot start)"
else
  [[ "$(doctor_row browser "${doctor}")" == ok ]] && pass "doctor: browser row is ok (installed by the installer)" \
    || fail "doctor: browser row is $(doctor_row browser "${doctor}"): $(jq -r '[.checks[] | select(.name == "browser")][0].detail' <<<"${doctor}")"
fi

# ============================================================ update (same tag)
section "--update to the same version keeps everything"
echo "keep me" | runuser -u praxi -- tee "${DATA}/e2e-marker" >/dev/null
before="$(sha256sum "${DATA}/api-token" /etc/praxi/praxi.env | cut -d' ' -f1 | tr '\n' ' ')"
if run_installer --update --version "${TAG_V1}" --file "/e2e/rel/v1/${ASSET}"; then pass "installer --update exited 0"; else show_log; fail "installer --update failed"; fi
installer_log_has "updating in place" && pass "reported an in-place update" || fail "no update line"
wait_healthy && pass "daemon is healthy again" || fail "daemon is not healthy after the update"
startup_note
check "praxi.service is active" systemctl is-active --quiet praxi.service
[[ "$(sha256sum "${DATA}/api-token" /etc/praxi/praxi.env | cut -d' ' -f1 | tr '\n' ' ')" == "${before}" ]] && pass "api-token and praxi.env (secret-store passphrase) are unchanged" || fail "token or env file changed"
[[ "$(cat "${DATA}/e2e-marker")" == "keep me" ]] && pass "data file survived" || fail "data file lost"
check "npm-installed CLI survived" as_praxi semver 1.2.3
check "port ${PORT} kept without --port" grep -Fxq "BOT_PORT=${PORT}" /etc/praxi/praxi.env
check "host mode kept" grep -Eq '^ExecStart=/opt/praxi/bin/bot serve --host$' /etc/systemd/system/praxi.service
check "the previous program files were kept for a rollback" bash -c 'ls -d /opt/praxi.backup-* >/dev/null'
[[ "$(api_code /v1/doctor -H "Authorization: Bearer $(cat "${DATA}/api-token")")" == 200 ]] && pass "the same token still works" || fail "token no longer accepted"
if [[ "${BROWSER_INSTALLED}" -eq 1 ]]; then
  check "the managed browser survived" test -d "${DATA}/browser"
fi

# ============================================================ update (newer)
section "--update to a newer tag"
if run_installer --update --version "${TAG_V2}" --file "/e2e/rel/v2/${ASSET}"; then pass "installer --update to ${TAG_V2} exited 0"; else show_log; fail "update to ${TAG_V2} failed"; fi
grep -Fq "${TAG_V2}" /opt/praxi/VERSION && pass "/opt/praxi/VERSION names ${TAG_V2}" || fail "VERSION: $(cat /opt/praxi/VERSION)"
wait_healthy && pass "daemon is healthy" || fail "daemon is not healthy"
[[ "$(cat "${DATA}/e2e-marker")" == "keep me" ]] && pass "data file survived" || fail "data file lost"

# ================================================================ uninstall
section "--uninstall (keeps data)"
if run_installer --uninstall; then pass "installer --uninstall exited 0"; else show_log; fail "uninstall failed"; fi
check_not "praxi.service is gone" systemctl cat praxi.service
check "no praxi unit files remain" bash -c '[[ -z "$(ls /etc/systemd/system | grep -E "^praxi")" ]]'
check "no praxi unit is loaded or failed" bash -c '[[ -z "$(systemctl list-units --all --plain --no-legend "praxi*")" ]]'
check_not "/opt/praxi is gone" test -e /opt/praxi
check_not "/opt/praxi-nodejs is gone" test -e /opt/praxi-nodejs
check_not "praxi wrapper is gone" test -e /usr/local/bin/praxi
check_not "nothing listens on ${PORT}" bash -c '[[ -n "$(ss -H -ltn "sport = :'"${PORT}"'")" ]]'
check "no praxi process is left" bash -c '! pgrep -u praxi >/dev/null'
check "data kept: ${DATA}/e2e-marker" test -f "${DATA}/e2e-marker"
check "config kept: /etc/praxi/praxi.env" test -f /etc/praxi/praxi.env
check "user kept" id praxi

section "--uninstall --purge"
if run_installer --uninstall --purge; then pass "installer --uninstall --purge exited 0"; else show_log; fail "purge failed"; fi
check_not "data directory is gone" test -e "${DATA}"
check_not "config directory is gone" test -e /etc/praxi
check_not "praxi user is gone" id praxi
check_not "praxi group is gone" getent group praxi

# ================================================================== summary
printf '\n'
if [[ "${FAILS}" -ne 0 ]]; then
  printf '%d check(s) FAILED\n' "${FAILS}" >&2
  exit 1
fi
printf 'all checks passed\n'
