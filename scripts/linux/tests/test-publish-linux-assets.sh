#!/usr/bin/env bash
# Exercise the GitHub CLI calls without credentials or network access.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
publisher="${repo_root}/scripts/release/publish-linux-assets.sh"
tmp="$(mktemp -d)"
trap 'rm -rf -- "${tmp}"' EXIT

export GITHUB_REPOSITORY=emreocell/Praxi-Bot
export DIST_DIR="${tmp}/dist" RUNNER_TEMP="${tmp}/runner"
export GH_CALLS="${tmp}/gh-calls" GH_NOTES="${tmp}/gh-notes"
export PATH="${tmp}/bin:${PATH}"
mkdir -p "${DIST_DIR}" "${RUNNER_TEMP}" "${tmp}/bin"
assets=("${DIST_DIR}/praxi-linux-x86_64.tar.gz" "${DIST_DIR}/praxi-linux-x86_64.tar.gz.sha256"
  "${DIST_DIR}/install-linux.sh" "${DIST_DIR}/praxi-linux.sigstore.json")
touch "${assets[@]}"

cat > "${tmp}/bin/gh" <<'GH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" >> "${GH_CALLS}"
case "$1 $2" in
  'release view')
    [[ -n "${EXISTING_ASSET:-}" ]] || exit 1
    printf '%s\n' "${EXISTING_ASSET}"
    ;;
  'release create')
    while (( $# )); do
      if [[ "$1" == --notes-file ]]; then
        cp "$2" "${GH_NOTES}"
        exit
      fi
      shift
    done
    exit 2
    ;;
  'release upload') ;;
  *) exit 2 ;;
esac
GH
chmod +x "${tmp}/bin/gh"
[[ "$(command -v gh)" == "${tmp}/bin/gh" ]]

expected_view() {
  printf '%s\n' release view "$1" --repo "${GITHUB_REPOSITORY}" --json assets --jq '.assets[].name'
}

check_create() {
  local tag="$1" previous="$2"
  shift 2
  local input=("${tag}")
  if [[ -n "${previous}" ]]; then
    input+=("${previous}")
  fi
  : > "${GH_CALLS}"
  EXISTING_ASSET='' bash "${publisher}" "${input[@]}"
  {
    expected_view "${tag}"
    printf '%s\n' release create "${tag}" "${assets[@]}" --verify-tag --generate-notes \
      --title "${tag}" --notes-file "${RUNNER_TEMP}/linux-release-notes.md" --repo "${GITHUB_REPOSITORY}" "$@"
  } > "${tmp}/expected"
  diff -u "${tmp}/expected" "${GH_CALLS}"
  grep -Fxq -- '### Verified Linux installation' "${GH_NOTES}"
  grep -Fq -- "version='${tag}'" "${GH_NOTES}"
  grep -Fq -- '--signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml' "${GH_NOTES}"
  if grep -Fq -- '__PRAXI_TAG__' "${GH_NOTES}"; then
    echo 'release notes still contain an unexpanded tag placeholder' >&2
    exit 1
  fi
}

check_create v0.1.0 ''
check_create v0.2.0-rc.1 v0.1.0 --prerelease --notes-start-tag v0.1.0
check_create v0.2.0 v0.1.0 --notes-start-tag v0.1.0
echo 'PASS: first, prerelease, and final release creation'

: > "${GH_CALLS}"
EXISTING_ASSET=Praxi-Setup.exe bash "${publisher}" v0.2.0 v0.1.0
{
  expected_view v0.2.0
  printf '%s\n' release upload v0.2.0 "${assets[@]}" --repo "${GITHUB_REPOSITORY}"
} > "${tmp}/expected"
diff -u "${tmp}/expected" "${GH_CALLS}"
echo 'PASS: existing release receives only asset uploads'

: > "${GH_CALLS}"
if EXISTING_ASSET=install-linux.sh bash "${publisher}" v0.2.0 v0.1.0 > "${tmp}/output" 2>&1; then
  echo 'publisher unexpectedly accepted a duplicate asset' >&2
  exit 1
fi
grep -Fq -- 'refusing to overwrite existing release asset install-linux.sh' "${tmp}/output"
expected_view v0.2.0 > "${tmp}/expected"
diff -u "${tmp}/expected" "${GH_CALLS}"
echo 'PASS: duplicate asset aborts before any upload'

: > "${GH_CALLS}"
for previous in refs/tags/v0.1.0 v0.2.0; do
  if bash "${publisher}" v0.2.0 "${previous}" > "${tmp}/output" 2>&1; then
    echo "publisher unexpectedly accepted previous tag ${previous}" >&2
    exit 1
  fi
  grep -Fxq -- "invalid previous release tag ${previous}" "${tmp}/output"
done
[[ ! -s "${GH_CALLS}" ]]
echo 'PASS: invalid previous tags abort before contacting GitHub'
