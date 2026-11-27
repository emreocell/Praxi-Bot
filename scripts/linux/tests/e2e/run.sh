#!/usr/bin/env bash
# End-to-end test of scripts/install-linux.sh on a fresh Ubuntu 24.04 with real systemd.
#
#   scripts/linux/tests/e2e/run.sh --bin-dir <dir with bot and bot-node> [--mode host-mode|headless]
#
# Packages the two binaries into a release-shaped tarball, boots a systemd container, installs from
# that local file and checks the result (checks.sh runs inside the container): services, `praxi
# doctor`, the authenticated API on loopback, the praxi wrapper, Node.js/npm without root, the browser
# repair, `--update` (same and newer tag) keeping data, `--uninstall` and `--uninstall --purge`.
#
# E2E_REQUIRE_BROWSER=1 makes the checks that need a started Chrome mandatory (CI sets it); without
# it they are skipped on a machine that restricts user namespaces (Ubuntu 24.04's default).
# E2E_KEEP=1 leaves the container running for inspection.
#
# Needs: docker (a daemon that allows --privileged), network for apt, nodejs.org, registry.npmjs.org
# and storage.googleapis.com. It never talks to GitHub releases.
#
# How the release trust check stays real: the installer is run UNMODIFIED. It only trusts
# /usr/bin/gh, so the test puts a test double there (e2e-gh.sh) that checks the same policy flags the
# installer passes and that the attestation bundle names the archive's real SHA-256 and the release
# tag. That tests the installer's enforcement (a tampered archive and a missing verifier are refused),
# not GitHub's or Sigstore's own cryptography. No installer flag exists for any of this.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${HERE}/../../../.." && pwd)"

BIN_DIR=""
MODE="host-mode"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin-dir) BIN_DIR="$2"; shift 2 ;;
    --mode) MODE="$2"; shift 2 ;;
    *) echo "usage: $0 --bin-dir <dir> [--mode host-mode|headless]" >&2; exit 2 ;;
  esac
done
[[ -x "${BIN_DIR}/bot" && -x "${BIN_DIR}/bot-node" ]] || { echo "error: --bin-dir must hold executable bot and bot-node" >&2; exit 2; }
case "${MODE}" in host-mode|headless) ;; *) echo "error: --mode is host-mode or headless" >&2; exit 2 ;; esac
command -v docker >/dev/null || { echo "error: docker is required" >&2; exit 2; }

case "$(uname -m)" in
  x86_64|amd64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
  *) echo "error: unsupported architecture $(uname -m)" >&2; exit 2 ;;
esac

IMAGE="praxi-e2e-ubuntu-systemd:24.04"
CONTAINER="praxi-e2e-${MODE}-$$"
WORK="$(mktemp -d)"
# shellcheck disable=SC2317,SC2329 # run through the EXIT trap
cleanup() {
  [[ "${E2E_KEEP:-0}" == 1 ]] || docker rm -f "${CONTAINER}" >/dev/null 2>&1 || true
  rm -rf "${WORK}"
}
trap cleanup EXIT

# ------------------------------------------------------------- release files
ASSET="praxi-linux-${ARCH}.tar.gz"
TAG_V1="v0.0.1-e2e.1"
TAG_V2="v0.0.1-e2e.2"

# Same layout as the "Package tarball" step of .github/workflows/linux-release.yml.
package() { # <tag> <out dir>
  local tag="$1" out="$2" stage="${WORK}/stage-${1}"
  mkdir -p "${stage}/praxi-linux-${ARCH}/bin" "${out}"
  install -m 0755 "${BIN_DIR}/bot" "${BIN_DIR}/bot-node" "${stage}/praxi-linux-${ARCH}/bin/"
  install -m 0644 "${REPO_ROOT}/LICENSE" "${REPO_ROOT}/NOTICE" "${REPO_ROOT}/THIRD_PARTY_NOTICES.md" "${stage}/praxi-linux-${ARCH}/"
  echo "${tag} e2e" > "${stage}/praxi-linux-${ARCH}/VERSION"
  tar -C "${stage}" -czf "${out}/${ASSET}" "praxi-linux-${ARCH}"
  (cd "${out}" && sha256sum "${ASSET}" > "${ASSET}.sha256")
  # The "attestation": which tag built which digest. e2e-gh.sh compares it with what the installer asks for.
  printf 'ref=refs/tags/%s\nsha256=%s\n' "${tag}" "$(sha256sum "${out}/${ASSET}" | cut -d' ' -f1)" > "${out}/praxi-linux.sigstore.json"
}
package "${TAG_V1}" "${WORK}/rel/v1"
package "${TAG_V2}" "${WORK}/rel/v2"

# A tampered archive whose checksum file was regenerated to match (a checksum is not proof of origin),
# next to the genuine bundle of v1.
mkdir -p "${WORK}/rel/tampered"
cp "${WORK}/rel/v1/praxi-linux.sigstore.json" "${WORK}/rel/tampered/"
cp "${WORK}/rel/v1/${ASSET}" "${WORK}/rel/tampered/${ASSET}"
printf 'tampered\n' | gzip -c >> "${WORK}/rel/tampered/${ASSET}"
(cd "${WORK}/rel/tampered" && sha256sum "${ASSET}" > "${ASSET}.sha256")

cp "${REPO_ROOT}/scripts/install-linux.sh" "${WORK}/install-linux.sh"
cp "${HERE}/checks.sh" "${HERE}/e2e-gh.sh" "${WORK}/"
chmod -R a+rX "${WORK}"

# ----------------------------------------------------------------- container
echo "== building the Ubuntu 24.04 systemd image"
docker build -q -t "${IMAGE}" "${HERE}" >/dev/null

echo "== booting systemd"
docker run -d --name "${CONTAINER}" --privileged --tmpfs /run --tmpfs /run/lock \
  -v "${WORK}:/e2e:ro" "${IMAGE}" >/dev/null
booted=0
for _ in $(seq 1 60); do
  state="$(docker exec "${CONTAINER}" systemctl is-system-running 2>/dev/null || true)"
  if [[ "${state}" == running || "${state}" == degraded ]]; then booted=1; break; fi
  sleep 1
done
[[ "${booted}" -eq 1 ]] || { docker logs "${CONTAINER}" | tail -30 >&2; echo "error: systemd did not boot in the container" >&2; exit 1; }

echo "== running the checks (${MODE})"
rc=0
docker exec -e "E2E_REQUIRE_BROWSER=${E2E_REQUIRE_BROWSER:-0}" -e "E2E_TAG_V1=${TAG_V1}" -e "E2E_TAG_V2=${TAG_V2}" -e "E2E_ARCH=${ARCH}" \
  "${CONTAINER}" bash /e2e/checks.sh "${MODE}" || rc=$?
if [[ "${rc}" -ne 0 ]]; then
  echo "== checks failed (rc=${rc}); journal of the Praxi services:" >&2
  docker exec "${CONTAINER}" journalctl -u 'praxi*' -n 60 --no-pager >&2 || true
fi
exit "${rc}"
