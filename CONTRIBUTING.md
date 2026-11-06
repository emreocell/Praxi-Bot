# Contributing

## Commit style

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(agent-core): add compaction step to agent loop
fix(tool-registry): handle duplicate tool names gracefully
docs: update build instructions
```

## Branch naming

Feature branches: `feat/<short-description>`
Bug fixes: `fix/<short-description>`

## Before opening a pull request

Run the full check suite locally and make sure all four commands pass:

```sh
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
```

No PR will be merged if any of these commands fail in CI.

## License

By contributing, you agree that your contribution is submitted under the
Apache-2.0 license (see LICENSE).
