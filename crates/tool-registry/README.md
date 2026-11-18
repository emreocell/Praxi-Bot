# tool-registry

The tool-registry crate defines what a tool is and keeps the runtime set of
registered tools. The agent loop looks tools up here by exact name, and
discovery builds its search over the registered specs.

## Entry points

- [`Tool`](src/tool.rs) is the async trait every tool implements: `spec()`
  describes it, `call(&ToolContext, Value)` runs it, and `visible_to` filters it
  per bot. `ToolOutput` is the result and `ToolContext` carries the call context.
- [`ToolSpec`](src/spec.rs) is the static description: name, description, JSON
  input schema, `Effect`, `PermissionCategory`, `Platform`s and a `ContentClass`.
  `ToolSpec::is_valid` checks the basic rules.
- [`ToolRegistry`](src/registry.rs) is a cheaply cloneable, thread-safe map.
  `register`, `unregister`, `get` and `list` manage the tools, and `generation`
  is a change counter that derived indexes use to detect staleness.
- [`ToolError`](src/error.rs) is the error type returned by tools.

## Invariants

A tool must declare its content classification. `ToolRegistry::register` returns
`false` for a tool whose `ContentClass` is `unspecified()`, and for a name that is
already registered, so an unclassified tool never becomes visible.

## Tests

From the workspace root, run `cargo test -p tool-registry`.
