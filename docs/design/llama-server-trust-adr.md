# ADR: trust model of the managed llama-server

Status: accepted. PR: #610. Code: `crates/provider-catalog/src/local_runtime/`
(`manifest.rs`, `download.rs`, `extract.rs`, `trusted.rs`, `serve.rs`).

## Context

One-click local models mean the daemon downloads a program (`llama-server`
from llama.cpp) and model files, then runs the program. A file that merely
exists in a folder must never be run just because it is there.

## Decision

- **Pinned manifest.** `manifest.rs` lists exact release assets and GGUF models
  with size and SHA-256 (https only; http only to loopback for tests).
  Downloads go to `<dest>.part`, resume with `Range`, and are renamed only when
  size and hash match. A wrong hash deletes the file.
- **Safe unpacking.** Zip-slip paths and symlinks/devices are refused or
  skipped, entry and size limits apply, files are created with `create_new`.
- **Fresh directory.** `install_verified` hashes the archive once on an open
  handle, checks it, and unpacks into a folder that must not exist yet
  (`create_dir` fails otherwise). It writes a manifest of the SHA-256 of every
  unpacked file next to it.
- **Verify before start.** `start_server` re-hashes the whole folder against
  that manifest. A changed, missing or extra file, or a missing manifest,
  means refuse and install again.
- **Network.** The server binds `127.0.0.1` only, with no web page. A busy port
  is reported, never taken over. The install routes (`/v1/local-install`) are
  owner-only (`OWNER_ROUTES`).

## Consequences

- Leftovers, partial installs and files from other sources are never run.
- **Residual risk, stated plainly:** the manifest sits in the same user-owned
  data folder, so something that can already write there as this user can forge
  it. A same-user process that swaps a file between the hash check and
  `Command::spawn` (one hashing pass wide) is not detected (TOCTOU).
- Upgrading llama.cpp means changing the pinned manifest in a release.
- CPU builds only; no GPU builds are offered yet.

## Alternatives rejected

- Trusting a binary because it exists or was downloaded earlier.
- Bundling llama.cpp in the installer: installer size and update lag.
- Verifying upstream signatures instead of pinned hashes: the manifest was
  built from release metadata, and no signature scheme was pinned.
