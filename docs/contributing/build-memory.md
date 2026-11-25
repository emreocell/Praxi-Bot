# Keeping debug and test builds light on memory

Debug and test builds of this workspace are heavy on Windows (MSVC): the linker alone can take
several gigabytes per test binary, and a target directory grows to tens of gigabytes. If you run
several builds at once, or a machine with 16 GB of RAM, this is usually what runs out first.

## What the workspace does about it

The root `Cargo.toml` sets

```toml
[profile.dev]
debug = "line-tables-only"
```

`[profile.test]` inherits from `dev`, so `cargo build`, `cargo test` and `cargo clippy` all use it.
CI already built this way (`CARGO_PROFILE_DEV_DEBUG` in `.github/workflows/ci.yml`); now local builds
match. `release` and the installer build do not use this profile and are unchanged.

You keep file and line numbers in panic backtraces. You lose full type and variable information in
a debugger (variables show as optimised out or without their types). If you need that for a session:

```powershell
$env:CARGO_PROFILE_DEV_DEBUG = 'true'   # full debug info for this shell only
cargo test -p <crate>
```

## Measured effect

`cargo test -p cli-provider --no-run` on Windows 11, MSVC, Rust 1.99.1, cold target directory,
`CARGO_INCREMENTAL=0`, `CARGO_BUILD_JOBS=2`, other builds running on the same machine (so wall time
is noisy, memory and size are not):

| | `debug = true` (old default) | `line-tables-only` |
|---|---|---|
| peak `link.exe` working set | 3112 MB | 487 MB |
| peak `rustc` working set | 1113 MB | 513 MB |
| wall time | 199 s | 112 s |
| target directory | 1.67 GB | 1.05 GB |
| `.pdb` files | 0.85 GB | 0.48 GB |

## Other things that help

- One cargo at a time (`CARGO_BUILD_JOBS=2`), and put `CARGO_TARGET_DIR` on a disk with space,
  not inside a synced folder.
- Build only what you touch: `cargo test -p <crate>` instead of `--workspace`.
- `CARGO_INCREMENTAL=0` keeps the target directory smaller at the cost of slower rebuilds.
- Delete a target directory when you are done with a branch.

## Tried and not adopted

- `[profile.dev.package."*"] debug = false` (no debug info at all for dependencies): about 10 % less
  disk on top, but backtraces lose file and line inside dependency frames, so it was left out.
