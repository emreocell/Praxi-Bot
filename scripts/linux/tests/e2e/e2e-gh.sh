#!/bin/bash
# Test double for the GitHub CLI, installed as /usr/bin/gh inside the e2e container only.
#
# `scripts/install-linux.sh` verifies a release with `gh attestation verify` and trusts only
# /usr/bin/gh. This double accepts exactly the policy the installer is meant to pass (the pinned repo,
# signer workflow, OIDC issuer, predicate type, github.com, no self-hosted runners, a tag source ref)
# and then requires the bundle to attest the archive's actual SHA-256 for that very tag. Anything else
# exits non-zero, so a tampered archive, a wrong tag or a weakened policy fails like with the real gh.
set -euo pipefail

if [[ "${1:-}" == --version ]]; then
  echo "gh version 2.80.0 (praxi e2e test double)"
  exit 0
fi
[[ "${1:-}" == attestation && "${2:-}" == verify && $# -ge 3 ]] || { echo "e2e gh: unexpected call: $*" >&2; exit 2; }
artifact="$3"
shift 3

declare -A flag=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --deny-self-hosted-runners) flag[$1]=1; shift ;;
    --bundle|--repo|--hostname|--signer-workflow|--source-ref|--cert-oidc-issuer|--predicate-type|--digest-alg)
      [[ $# -ge 2 ]] || { echo "e2e gh: $1 needs a value" >&2; exit 2; }
      flag[$1]="$2"; shift 2 ;;
    *) echo "e2e gh: unexpected option $1" >&2; exit 2 ;;
  esac
done

want() { # <flag> <value>
  [[ "${flag[$1]:-}" == "$2" ]] || { echo "e2e gh: $1 is '${flag[$1]:-}', expected '$2'" >&2; exit 1; }
}
want --repo emreocell/Praxi-Bot
want --hostname github.com
want --signer-workflow github.com/emreocell/Praxi-Bot/.github/workflows/linux-release.yml
want --cert-oidc-issuer https://token.actions.githubusercontent.com
want --predicate-type https://slsa.dev/provenance/v1
want --digest-alg sha256
want --deny-self-hosted-runners 1
[[ "${flag[--source-ref]:-}" =~ ^refs/tags/v[0-9] ]] || { echo "e2e gh: the source ref must be a release tag" >&2; exit 1; }

bundle="${flag[--bundle]:-}"
[[ -f "${bundle}" ]] || { echo "e2e gh: no bundle" >&2; exit 1; }
attested_ref="$(sed -n 's/^ref=//p' "${bundle}")"
attested_sha="$(sed -n 's/^sha256=//p' "${bundle}")"
actual_sha="$(sha256sum "${artifact}" | cut -d' ' -f1)"
[[ "${attested_ref}" == "${flag[--source-ref]}" ]] || { echo "e2e gh: bundle attests ${attested_ref}, not ${flag[--source-ref]}" >&2; exit 1; }
[[ "${attested_sha}" == "${actual_sha}" ]] || { echo "e2e gh: bundle attests ${attested_sha}, the archive is ${actual_sha}" >&2; exit 1; }
echo "e2e gh: verified ${artifact} for ${attested_ref}"
