# Secret scanning and test fixtures

GitHub secret scanning reads every file. Fake keys that look real raise alerts,
and a pile of harmless alerts can hide a real one. So:

- `.github/secret_scanning.yml` skips only `crates/secret-store/src/rules.rs`.
  That file is generated from the gitleaks rules (`scripts/tools/gen_secret_rules.py`)
  and holds gitleaks' public sample strings.
- Everywhere else, never write a realistic Google API key (`AIza` + 35 characters)
  or Telegram bot token (`123456789:` + 35 characters) as one literal. Build it
  at runtime so the shape is still tested but no scanner sees it:
  - Rust: `concat!("AIza", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")` or `format!`
  - TypeScript: `"AIza" + "A".repeat(35)`
- The test `crates/secret-store/tests/no_scannable_secret_literals.rs` fails when a
  new literal of those shapes appears in a tracked file outside the allow-list.
