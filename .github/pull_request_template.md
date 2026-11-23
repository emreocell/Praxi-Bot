## What and why

<!-- What does this change, and why? Link the issue: Closes #123 -->

<!-- Suggest a release notes label if it fits the primary change: bug for a fix, enhancement for a feature, or tests for test-only work. Maintainers apply the label before merging. -->

## How it was tested

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy -p <crates you touched> --all-targets -- -D warnings`
- [ ] `cargo test -p <crates you touched>`
- [ ] `pnpm typecheck`, `pnpm lint` and `pnpm --dir apps/desktop/ui test` (UI changes)
- [ ] `pnpm --dir apps/desktop/ui check:i18n` and `check:themes` (UI text or styles); new strings are really translated in en, tr and de
- [ ] `cargo test -p evals` (a safety gate or a tool that reads untrusted content changed; scenario added or updated)
- [ ] `python3 scripts/check_markdown_links.py` (docs or README changed)
- [ ] `cargo deny check` (dependencies changed)
- [ ] Other (describe): 

## Screenshots (UI changes)

<!-- Mock mode (?mock=1): at least one dark theme and the light theme; all three languages (en, tr, de) if text changed. -->

## Generated files

- [ ] No API change, or `docs/api/openapi.json` and `apps/desktop/ui/src/api/schema.d.ts` were regenerated with the repo scripts (never hand-edited)

## Security-sensitive areas touched

<!-- Tick what applies; maintainers review these more closely. See CONTRIBUTING.md. -->

- [ ] Approvals / permissions / the never-list
- [ ] The egress gate
- [ ] Secrets or redaction
- [ ] Pairing, host administration or owner-only routes (new routes are classified in `owner.rs`)
- [ ] File protected paths or the shell tool
- [ ] None of the above

## Notes for the reviewer

<!-- Not done / not verified / anything surprising. -->
