# Deferred tool loading

Status: accepted.

The shared model request has a budget of 40 tool definitions. The installed
catalog includes built-ins and can grow with MCP connections and agent tools.
The existing BM25 index already searches the registry, but its dispatch
wrapper makes the model reconstruct arguments from text and gives providers no
schema for that call.

## Decision

- Start each run with a curated set of common tools and the two discovery
  operations. Use the same catalog visibility check as for direct calls.
  `tool_search` returns bounded, ranked records with canonical name, a
  one-line description and classification. `tool_describe` accepts an exact
  canonical name and adds that tool's definition to subsequent model requests.
- Keep the initial tool order deterministic. Append activated definitions in
  activation order so a new activation preserves the existing tool prefix.
  Once the request budget is full, report that capacity to the model. Keep
  successful activations in the run's durable tool-step results so resuming a
  run can reconstruct the same surface. Recheck visibility on every request
  and dispatch.
- Use the existing shared request schema for every provider. Names outside the
  portable function-name character set receive deterministic aliases at the
  provider boundary. Resolve aliases back to canonical registry names before
  policy evaluation. Use the curated direct surface for the lightweight
  executor tier. Profiles can opt into that set for other limited models
  with `direct_tools_only = true` under `[models]`.
- Keep the older discovery names for existing transcript replay and CLI
  engines that negotiate a static MCP tool list per process. HTTP model
  requests advertise the two discovery operations and call activated tools
  through their typed definitions.

## Trust boundaries

Activation changes schema availability. The actual call still resolves to the
registered tool and goes through its policy, approval, egress, taint, and
tool-specific checks. An unknown name and a name hidden by policy return the
same result. Search and activation cannot invoke their target. Reject unsafe
identifier text, bound search summaries, and classify any external metadata
shown to the model as untrusted. Approval text continues to use daemon-owned
labels and the actual call arguments.
