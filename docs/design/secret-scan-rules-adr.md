# ADR: secret scanning rules derived from gitleaks

Status: accepted. PRs: #606, #646. Code: `crates/secret-store/src/{scan,rules,redact}.rs`,
`scripts/tools/gen_secret_rules.py`. See also
[secret scanning for contributors](../contributing/secret-scanning.md).

## Context

Warning before a secret is sent, and masking it in logs, needs broad coverage
of real key formats. Hand-written regexes covered a handful. gitleaks (MIT)
maintains a large rule set.

## Decision

- `gen_secret_rules.py` converts the gitleaks `config/gitleaks.toml` (pinned
  commit in the file header) into the generated `rules.rs`: id, description,
  regex, keywords, entropy, secret group and secret-level allow-list. Nobody
  edits it by hand; re-run the generator to update.
- `scan_text` runs one Aho-Corasick keyword pass to pick candidate rules,
  compiles regexes lazily, checks entropy on the secret and merges overlaps. A
  `Finding` is rule id, description and byte span, never the value.
- `Redactor` masks through the same engine. The hand-tuned `SHAPES` list stays
  as a floor for formats gitleaks lacks (for example `sk-or-v1-`, `nvapi-`).
- Left out of the table: path-gated rules (we scan text, not files),
  `generic-api-key` (too noisy for prose) and allow-lists that are not about
  the secret itself.
- The generator rewrites `\w`, `\d`, `\b` to ASCII (Go RE2 meaning), which
  compiles and is faster than Unicode classes.
- Findings warn; they do not block.
- **Allow-list for GitHub's scanner:** `.github/secret_scanning.yml` skips only
  `rules.rs`, whose samples are public gitleaks examples. Elsewhere, test keys
  are built at runtime (`concat!`, `"AIza" + "A".repeat(35)`), and
  `tests/no_scannable_secret_literals.rs` fails on a new literal.

## Consequences

- Wide coverage, with a licence header and a `THIRD_PARTY_NOTICES.md` entry.
- Real-world noise of the rules is not measured and the false-positive corpus
  is small. Rules are not auto-synced with newer gitleaks.
- `detect.rs` and `scan.rs` overlap in purpose; merging them is left open.

## Alternatives rejected

- Linking gitleaks (Go) or a GPL scanner: licence and packaging.
- Ignoring whole directories in GitHub scanning: it would hide a real leak.
