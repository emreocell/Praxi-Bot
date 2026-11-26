#!/usr/bin/env bash
# Hermetic tests for crates/host-provision/scripts/bootstrap.sh (the one-command server setup).
# The script is only sourced: its helper functions are exercised directly, nothing is installed,
# no network is used and no root is needed. GPG tests use throwaway keys in a temporary home.

set -euo pipefail

if [[ "${OSTYPE:-}" != linux* ]]; then
  echo "SKIP: test-bootstrap.sh runs only on Linux (OSTYPE=${OSTYPE:-unset}); use CI" >&2
  exit 0
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
BOOTSTRAP="${REPO_ROOT}/crates/host-provision/scripts/bootstrap.sh"

TMP_ROOT="$(mktemp -d)"
cleanup() {
  if [[ -d "${TMP_ROOT}/gnupg" ]]; then
    gpgconf --homedir "${TMP_ROOT}/gnupg" --kill gpg-agent >/dev/null 2>&1 || true
  fi
  rm -rf "${TMP_ROOT}"
}
trap cleanup EXIT

pass() { printf 'PASS: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

bash -n "${BOOTSTRAP}" || fail "bootstrap.sh has a syntax error"
# shellcheck source=/dev/null
source "${BOOTSTRAP}"
pass "bootstrap.sh sources without running anything"

# 1. Every https download refuses a redirect to another scheme.
mapfile -t curls < <(sed -e ':a' -e '/\\$/N; s/\\\n[[:space:]]*/ /; ta' "${BOOTSTRAP}" | grep -E '(^|[^[:alnum:]_])curl .*(https://|\$\{base\})')
[[ "${#curls[@]}" -ge 4 ]] || fail "expected at least four curl downloads, found ${#curls[@]}"
for line in "${curls[@]}"; do
  [[ "${line}" == *"--proto '=https'"* && "${line}" == *"--proto-redir '=https'"* ]] || fail "curl without --proto/--proto-redir '=https': ${line}"
done
pass "every https curl pins the protocol and the redirect protocol"

# 2. The installer is started only through the clean-environment helper, after the signature check.
flat="$(sed -e ':a' -e '/\\$/N; s/\\\n[[:space:]]*/ /; ta' "${BOOTSTRAP}")"
! grep -Eq 'bash "\$\{work\}/install-linux.sh"' <<<"${flat}" || fail "the installer must not be started directly"
verify_line="$(grep -n '/usr/bin/gh attestation verify' <<<"${flat}" | head -n1 | cut -d: -f1)"
# shellcheck disable=SC2016 # the patterns name the script's own ${work} text literally
run_line="$(grep -n 'praxi_run_installer "\${work}/install-linux.sh"' <<<"${flat}" | head -n1 | cut -d: -f1)"
[[ -n "${verify_line}" && -n "${run_line}" && "${verify_line}" -lt "${run_line}" ]] || fail "the signature check must come before the installer run"
grep -Eq 'praxi_clean_env "\$\{work\}"[[:space:]]+/usr/bin/gh attestation verify' <<<"${flat}" || fail "the signature check must run through praxi_clean_env with /usr/bin/gh"
pass "signature check (clean env, /usr/bin/gh) precedes a clean-env installer run"

# 3. The clean environment keeps nothing from the caller but proxy settings.
HOME_DIR="${TMP_ROOT}/home"
mkdir -p "${HOME_DIR}" "${TMP_ROOT}/planted"
printf '#!/bin/sh\necho planted > "%s/planted-ran"\n' "${TMP_ROOT}" > "${TMP_ROOT}/planted/gh"
chmod +x "${TMP_ROOT}/planted/gh"
env_out="$(
  PATH="${TMP_ROOT}/planted:${PATH}" HOME=/caller/home GH_CONFIG_DIR=/caller/gh GH_TOKEN=caller-token \
    GITHUB_TOKEN=caller-github-token LD_LIBRARY_PATH=/caller/lib BASH_ENV=/caller/evil.sh \
    HTTPS_PROXY=http://proxy.invalid:3128 praxi_clean_env "${HOME_DIR}" /usr/bin/env
)"
grep -qx 'PATH=/usr/sbin:/usr/bin:/sbin:/bin' <<<"${env_out}" || fail "PATH is not the fixed system path: ${env_out}"
grep -qx "HOME=${HOME_DIR}" <<<"${env_out}" || fail "HOME is not isolated"
grep -qx "GH_CONFIG_DIR=${HOME_DIR}/gh-config" <<<"${env_out}" || fail "GH_CONFIG_DIR is not isolated"
grep -qx 'HTTPS_PROXY=http://proxy.invalid:3128' <<<"${env_out}" || fail "proxy settings must be kept"
for leaked in caller-token caller-github-token /caller/ LD_LIBRARY_PATH BASH_ENV "${TMP_ROOT}/planted"; do
  ! grep -qF -- "${leaked}" <<<"${env_out}" || fail "the clean environment leaked ${leaked}"
done
pass "praxi_clean_env: fixed PATH, isolated HOME and gh config, no tokens or loader variables"

# 4. The installer starts with that environment and never finds a gh planted in the caller's PATH.
cat > "${TMP_ROOT}/install-linux.sh" <<'EOF'
#!/usr/bin/env bash
{
  printf 'PATH=%s\n' "${PATH:-}"
  printf 'HOME=%s\n' "${HOME:-}"
  printf 'GH_CONFIG_DIR=%s\n' "${GH_CONFIG_DIR:-}"
  printf 'GH_TOKEN=%s\n' "${GH_TOKEN:-}"
  printf 'DEBIAN_FRONTEND=%s\n' "${DEBIAN_FRONTEND:-}"
  printf 'GH=%s\n' "$(command -v gh || echo none)"
  printf 'ARGS=%s\n' "$*"
} > "$(dirname "$0")/installer-saw"
EOF
PATH="${TMP_ROOT}/planted:${PATH}" HOME=/caller/home GH_CONFIG_DIR=/caller/gh GH_TOKEN=caller-token \
  praxi_run_installer "${TMP_ROOT}/install-linux.sh" v1.2.3
saw="${TMP_ROOT}/installer-saw"
grep -qx 'PATH=/usr/sbin:/usr/bin:/sbin:/bin' "${saw}" || fail "the installer did not get the fixed PATH"
grep -qx "HOME=${TMP_ROOT}" "${saw}" || fail "the installer did not get an isolated HOME"
grep -qx "GH_CONFIG_DIR=${TMP_ROOT}/gh-config" "${saw}" || fail "the installer did not get an isolated gh config"
grep -qx 'GH_TOKEN=' "${saw}" || fail "the caller's GH_TOKEN reached the installer"
grep -qx 'DEBIAN_FRONTEND=noninteractive' "${saw}" || fail "the installer must run non-interactively"
grep -qx 'ARGS=--version v1.2.3 --headless' "${saw}" || fail "unexpected installer arguments: $(grep '^ARGS=' "${saw}")"
! grep -qF "${TMP_ROOT}/planted" "${saw}" || fail "the installer could resolve a gh planted in the caller's PATH"
[[ ! -e "${TMP_ROOT}/planted-ran" ]] || fail "a planted gh was executed"
pass "praxi_run_installer: clean environment, planted gh unreachable"

# 5. Only the pinned GitHub CLI signing key is installed, whatever else the download holds.
if command -v gpg >/dev/null 2>&1 && command -v gpgconf >/dev/null 2>&1; then
  GNUPG="${TMP_ROOT}/gnupg"
  mkdir -m 700 "${GNUPG}"
  for who in pinned other; do
    gpg --homedir "${GNUPG}" --batch --quiet --pinentry-mode loopback --passphrase '' \
      --quick-generate-key "Praxi test ${who} <${who}@example.invalid>" ed25519 sign never >/dev/null 2>&1
  done
  fpr_of() { gpg --homedir "${GNUPG}" --batch --with-colons --fingerprint "$1" 2>/dev/null | awk -F: '$1 == "fpr" {print $10; exit}'; }
  pinned_fpr="$(fpr_of pinned@example.invalid)"
  other_fpr="$(fpr_of other@example.invalid)"
  [[ "${pinned_fpr}" =~ ^[0-9A-F]{40}$ && "${other_fpr}" =~ ^[0-9A-F]{40}$ && "${pinned_fpr}" != "${other_fpr}" ]] || fail "could not create the test keys"
  gpg --homedir "${GNUPG}" --batch --export pinned@example.invalid other@example.invalid > "${TMP_ROOT}/download.gpg"

  # shellcheck disable=SC2034 # read by praxi_pin_gh_key
  praxi_gh_key_fpr="${pinned_fpr}"
  praxi_pin_gh_key "${TMP_ROOT}/download.gpg" "${TMP_ROOT}/pinned.gpg" || fail "the pinned key was not accepted"
  mkdir -m 700 "${TMP_ROOT}/verify-home"
  listed="$(gpg --homedir "${TMP_ROOT}/verify-home" --batch --with-colons --show-keys "${TMP_ROOT}/pinned.gpg" 2>/dev/null | awk -F: '$1 == "fpr" {print $10}')"
  [[ "${listed}" == "${pinned_fpr}" ]] || fail "the output keyring must hold exactly the pinned key, got: ${listed}"
  pass "the pinned key is exported alone; other keys in the download are dropped"

  # shellcheck disable=SC2034 # read by praxi_pin_gh_key
  praxi_gh_key_fpr="${other_fpr}"
  rm -f "${TMP_ROOT}/pinned.gpg"
  gpg --homedir "${GNUPG}" --batch --export pinned@example.invalid > "${TMP_ROOT}/only-pinned.gpg"
  if praxi_pin_gh_key "${TMP_ROOT}/only-pinned.gpg" "${TMP_ROOT}/pinned.gpg"; then
    fail "a download without the pinned key must be refused"
  fi
  [[ ! -e "${TMP_ROOT}/pinned.gpg" ]] || fail "a refused key must not leave an output file"
  printf 'not a key\n' > "${TMP_ROOT}/garbage.gpg"
  if praxi_pin_gh_key "${TMP_ROOT}/garbage.gpg" "${TMP_ROOT}/pinned.gpg"; then
    fail "a file that is not a key must be refused"
  fi
  pass "a download without the pinned key (or not a key at all) is refused"
else
  echo "SKIP: gpg is not installed; key pinning is not exercised"
fi

# 6. The pinned fingerprint is a full 40-hex fingerprint, not a short or long key ID.
[[ "$(sed -n 's/^praxi_gh_key_fpr=\([0-9A-F]*\)$/\1/p' "${BOOTSTRAP}")" =~ ^[0-9A-F]{40}$ ]] || fail "the pinned GitHub CLI key must be a full 40-hex fingerprint"
pass "the pinned GitHub CLI key is a full fingerprint"

echo "ALL BOOTSTRAP HERMETIC TESTS PASSED"
