#!/usr/bin/env bash
# Publish the verified Linux assets of ONE tag to that tag's GitHub release (used by the
# "Linux Server Packages" workflow after the provenance was generated and verified).
#   publish-linux-assets.sh <tag> [previous-tag]  assets are read from ./dist (or $DIST_DIR)
#
# - A release that does not exist yet is created (pre-release for a suffixed tag).
# - A release that exists (the Windows installer is attached first) gets the Linux assets added.
# - Nothing is ever replaced: an asset name that already exists aborts the run, and no overwrite
#   flag is used anywhere in this file.
set -euo pipefail

if (( $# < 1 || $# > 2 )) || [[ -z "${1}" ]]; then
  echo 'usage: publish-linux-assets.sh <tag> [previous-tag]' >&2
  exit 1
fi
tag="${1}"
previous_tag="${2:-}"
repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}"
[[ "${repo}" == emreocell/Praxi-Bot ]] || { echo "refusing to publish for ${repo}" >&2; exit 1; }
tag_pattern='^v[0-9][0-9A-Za-z._+-]*$'
[[ "${tag}" =~ ${tag_pattern} ]] || { echo "invalid release tag ${tag}" >&2; exit 1; }
if [[ -n "${previous_tag}" ]] && [[ ! "${previous_tag}" =~ ${tag_pattern} || "${previous_tag}" == "${tag}" ]]; then
  echo "invalid previous release tag ${previous_tag}" >&2
  exit 1
fi
dist="${DIST_DIR:-dist}"
work="${RUNNER_TEMP:-$(mktemp -d)}"
assets=("${dist}"/*.tar.gz "${dist}"/*.sha256 "${dist}/install-linux.sh" "${dist}/praxi-linux.sigstore.json")
for asset in "${assets[@]}"; do [[ -f "${asset}" ]] || { echo "missing ${asset}" >&2; exit 1; }; done
if gh release view "${tag}" --repo "${repo}" --json assets --jq '.assets[].name' \
    > "${work}/existing-assets" 2>/dev/null; then
  for asset in "${assets[@]}"; do
    if grep -Fxq -- "$(basename "${asset}")" "${work}/existing-assets"; then
      echo "refusing to overwrite existing release asset $(basename "${asset}"); use a new version" >&2
      exit 1
    fi
  done
  gh release upload "${tag}" "${assets[@]}" --repo "${repo}"
  exit 0
fi

notes="${work}/linux-release-notes.md"
cat > "$notes" <<'NOTES'
### Verified Linux installation

Install trusted GitHub CLI >= 2.80.0 independently from https://cli.github.com.
Copy the bootstrap from a reviewed copy of `scripts/install-linux.sh`
or this workflow. Verify the installer before executing it:

```bash
(
  set -euo pipefail
  version='__PRAXI_TAG__'
  work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
  base="https://github.com/emreocell/Praxi-Bot/releases/download/$version"
  for asset in install-linux.sh praxi-linux.sigstore.json; do
    curl -fSL --proto '=https' --proto-redir '=https' --tlsv1.2 "$base/$asset" -o "$work/$asset"
  done
  gh attestation verify "$work/install-linux.sh" --bundle "$work/praxi-linux.sigstore.json" \
    --repo emreocell/Praxi-Bot --hostname github.com \
    --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml \
    --source-ref "refs/tags/$version" --deny-self-hosted-runners \
    --cert-oidc-issuer https://token.actions.githubusercontent.com \
    --predicate-type https://slsa.dev/provenance/v1 --digest-alg sha256
  sudo bash "$work/install-linux.sh" --version "$version"
)
```

The installer verifies the archive with the same pinned identity before
installing packages or changing services. `--verify-only` performs the
checks without installing. Local archives require `--version <tag>`
and the companion `praxi-linux.sigstore.json` bundle (or `--bundle <file>`).
A matching `.sha256` is an integrity check, never proof of origin.
For offline use, independently acquire `gh attestation trusted-root > trusted_root.jsonl`
on an online trusted machine, then transfer it with the artifact and bundle.
Pass `--offline --custom-trusted-root /trusted/trusted_root.jsonl` along with
`--file` and `--version`; installation also requires `--skip-packages`.
Releases without signed provenance are rejected; there is no bypass.
NOTES
sed -i "s/__PRAXI_TAG__/${tag}/g" "${notes}"
create_flags=(--verify-tag --generate-notes --title "${tag}" --notes-file "${notes}" --repo "${repo}")
# A tag with a suffix (v0.1.0-alpha.1) is a pre-release: it must not become "latest".
[[ "${tag}" == *-* ]] && create_flags+=(--prerelease)
[[ -n "${previous_tag}" ]] && create_flags+=(--notes-start-tag "${previous_tag}")
gh release create "${tag}" "${assets[@]}" "${create_flags[@]}"
