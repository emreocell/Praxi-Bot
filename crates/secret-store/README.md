# secret-store

The secret-store crate keeps API keys and tokens out of config files, logs and
the agent's context. It stores secrets by name, hands out name-only references,
and masks secret values in any text that might reach the model or the UI.

## Entry points

- [`SecretStore`](src/secret.rs) is the `get` / `set` / `delete` / `list_names`
  trait. Backends: `KeyringStore` (OS keychain), `EncryptedFileStore`
  (ChaCha20-Poly1305 with an Argon2id-derived key, for headless servers) and
  `MemoryStore` (in-process, for tests).
- [`open_store`](src/open.rs) picks a backend through `BackendChoice`
  (`Auto`, `Keyring` or `File`). The encrypted file is `secrets.enc` in the data
  directory and its passphrase comes from `BOT_SECRETS_PASSPHRASE`.
- [`SecretRef` and `resolve`](src/secret.rs) turn a name into a `SecretValue`.
  Call `resolve` only at the provider or MCP boundary.
- [`Redactor`](src/redact.rs) masks known secret values (including their JSON,
  URL and base64 forms) and common key shapes, replacing them with `[REDACTED]`.
- [`FillSecrets`](src/fill.rs) holds the owner-only secrets used by the browser
  fill tools.

## Invariants

A secret value must never leak through config, `Debug` or logs. `SecretValue`'s
`Debug` prints `SecretValue(<redacted>)` and its bytes are wiped on drop.
`SecretRef` holds only a name, and `SecretRef::new` and `resolve` refuse the
`fill/` namespace, reserved names, host credentials and the OAuth records the MCP
bridge manages, so configuration cannot reference them.

## Tests

From the workspace root, run `cargo test -p secret-store`. This includes the
integration tests in `tests/file_transactions.rs`; the keychain backend tests
depend on the platform's keychain.
