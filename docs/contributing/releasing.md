# Releasing Praxi-Bot

Use the full release tag for GitHub releases, build provenance and artifact selection. For example,
`v0.1.0-alpha.2` identifies the release and tells the Windows installer which Linux host packages
to install. Its package version is `0.1.0`. Another alpha of the same numeric version can keep those
package versions.

## Check package versions

For a numeric version bump, update `[workspace.package].version` in `Cargo.toml` and the repeated
version requirements on local Cargo dependencies that use the workspace version. Run
`cargo update --workspace` to refresh the corresponding entries in `Cargo.lock`. Match that numeric
version in `apps/desktop/tauri.conf.json`, `apps/desktop/ui/package.json`, and `packages/ui/package.json`.
The pnpm lockfile carries dependency resolution. Merge the version changes and wait for CI before
cutting the tag.

The checker uses Python 3.11 or newer and accepts a full tag or a bare numeric version. Run it from
the repository root, or pass `--root PATH` to check a different checkout.

```bash
python scripts/release/check_version.py v0.1.0-alpha.4
```

## Rehearse the release notes locally

Run this in Bash from the root of a full clone with the current upstream release tags. Use a clone
without extra local release tags so the range matches CI. Check out the intended release commit
first. Its full SHA must already be reachable from upstream `main`, where the release notes
configuration must also be available. Authenticate
GitHub CLI for `github.com`. A fine-grained token needs **Contents: write** on
`emreocell/Praxi-Bot` for the generate-notes endpoint. [GitHub REST API documentation](https://docs.github.com/en/rest/releases/releases#generate-release-notes-content-for-a-release)

```bash
(
  set -euo pipefail
  repo=emreocell/Praxi-Bot
  tag=v0.1.0-alpha.4  # Set the candidate tag.
  commit=$(git rev-parse --verify 'HEAD^{commit}')
  if [[ -n "$(git status --porcelain)" ]]; then
    echo 'Use a clean checkout of the intended release commit.' >&2
    exit 1
  fi
  git fetch --no-tags "https://github.com/$repo.git" refs/heads/main
  if ! git merge-base --is-ancestor "$commit" FETCH_HEAD; then
    echo 'The release commit must already be reachable from upstream main.' >&2
    exit 1
  fi
  python scripts/release/check_version.py "$tag"

  note_fields=(-f "tag_name=$tag" -f "target_commitish=$commit")
  previous_tag=$(git describe --tags --abbrev=0 --always --match 'v[0-9]*' --exclude "$tag" "$commit")
  if [[ "$previous_tag" == v* ]]; then
    note_fields+=(-f "previous_tag_name=$previous_tag")
  else
    previous_tag=''
  fi
  printf 'Tag %s, commit %s, previous tag %s\n' "$tag" "$commit" "${previous_tag:-<none>}" >&2
  gh api --method POST "repos/$repo/releases/generate-notes" "${note_fields[@]}" --jq '.body'
)
```

The command prints the grouped Markdown using [the repository's notes configuration](../../.github/release.yml).
It leaves releases, tags and assets untouched. Use an unpublished tag name for the rehearsal and
review the output, including whether it covers the expected PRs and places them in the intended
sections. The baseline is the nearest reachable release tag, the same selection used by the Linux
release workflow. With no such tag, the request leaves `previous_tag_name` unset.

For a grouping test, use merged PRs with the existing `bug`, `enhancement`, `documentation`,
`platform: windows`, `platform: linux` or `platform: macos` labels. The **Tests** section
uses `tests`. If that label is missing, a maintainer can create it under **Issues > Labels**
and apply it to a test PR. A test repository needs the same label names and notes configuration.
Unlabelled changes should appear in **Other changes**.

## Cut and publish the reviewed tag

After the version check and notes review, use the exact tag and full commit SHA printed by the
rehearsal. Run these commands only when ready to release. A pre-release tag push starts the
publishing workflow.

```bash
(
  set -euo pipefail
  tag=v0.1.0-alpha.4
  commit=REPLACE_WITH_REVIEWED_FULL_SHA
  git tag -a "$tag" "$commit" -m "$tag"
  git push https://github.com/emreocell/Praxi-Bot.git "refs/tags/$tag:refs/tags/$tag"
)
```

- **Pre-release.** A suffixed tag such as `v0.1.0-alpha.2` triggers the Linux build,
  attestation, verification, and publication. One maintainer can publish it. Run **Linux Server
  Packages** manually on `main` with `tag` set and `dry_run=false` to publish Linux assets for an
  existing pre-release tag.
- **Final release.** A suffix-free tag such as `v1.0.0` requires a protected tag and the
  `release` environment with a required reviewer other than the tag pusher and **Prevent
  self-review** enabled. Dispatch **Linux Server Packages** on the exact tag with
  `dry_run=false` and obtain the environment approval.

See [Releasing the Linux server packages](../guide/release-linux.md) for the publication rules.
The Linux workflow's `dry_run=true` exercises build, attestation and verification against an
existing tag without publishing assets. Use the local notes rehearsal to review a candidate before
creating the tag. [Release administrator setup and dry run](../guide/remote-host.md#release-administrator-setup-and-dry-run)

### Windows installer

**Windows Installer** builds the NSIS `*-setup.exe` for the tag and, for every tag push or manual
run on `main` in this repository, attests everything it publishes (SLSA build provenance through
`actions/attest-build-provenance`). Its artifacts:

| Artifact | Contents |
| --- | --- |
| `praxi-bot-windows-installer` | the `*-setup.exe` only (as before) |
| `praxi-bot-windows-release` | the installer, the `bot` sidecar it contains, three CycloneDX SBOMs, `build-info.txt`, `verify-windows.ps1` and `SHA256SUMS.txt` |
| `praxi-bot-windows-provenance` | `praxi-bot-windows.sigstore.json`, the attestation bundle for every file of the release artifact |

For a **pre-release tag** (`v1.2.3-alpha.1`) the last job, *Attach to the pre-release*, uploads
that set to the tag's GitHub release **if the release already exists**. It never creates a release
and never replaces an asset: a file already on the release must be byte-identical or the job
fails. If the release does not exist yet, the job summary prints the `gh run download` and
`gh release create` commands that create it with the full set. For a **final tag** nothing is
attached automatically: download both artifacts and attach them yourself with the same commands
after the review. Do not attach only the installer: users are told to verify it with
`SHA256SUMS.txt` and the bundle ([Check your download](../guide/verify-download.md)).

No signing certificate exists, so the installer itself is unsigned; provenance and checksums are
the only integrity signal today. Builds are not bit-for-bit reproducible (see the page above for
what is pinned and what is not).

If you publish the pre-release with the installer before Linux
publication, create it using the reviewed tag and previous tag from the rehearsal. Leave
`previous_tag` empty if the rehearsal had no baseline.

```bash
(
  set -euo pipefail
  repo=emreocell/Praxi-Bot
  tag=v0.1.0-alpha.4
  previous_tag=''  # Set to the previous tag printed by the rehearsal, if any.
  # After `gh run download <run-id> --name praxi-bot-windows-release --name praxi-bot-windows-provenance --dir release-files`
  files=(./release-files/*)
  notes_flags=(--generate-notes)
  if [[ -n "$previous_tag" ]]; then
    notes_flags+=(--notes-start-tag "$previous_tag")
  fi
  gh release create "$tag" "${files[@]}" --repo "$repo" --verify-tag --prerelease "${notes_flags[@]}"
)
```

The Linux publisher can add its verified assets to that release and preserves the existing body.
If Linux creates the release first, upload the Windows files to the existing release with
`gh release upload "$tag" ./release-files/* --repo emreocell/Praxi-Bot`, using the exact tag
(the installer workflow's *Attach to the pre-release* job does this by itself for pre-release
tags). Both paths preserve existing assets. [GitHub CLI release documentation](https://cli.github.com/manual/gh_release_create)
