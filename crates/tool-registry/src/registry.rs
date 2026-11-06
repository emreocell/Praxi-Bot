//! Runtime tool registry with substring-scored search.
//!
//! The search algorithm is a simple TF-style substring score.  BM25 will be
//! added in `agent-router` once an inverted index is available.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::RwLock;

use crate::{spec::ToolSpec, tool::Tool};

/// A registered tool entry.
struct Entry {
    tool: Arc<dyn Tool>,
}

/// Central registry for all available tools.
///
/// Thread-safe; clone the registry freely — all clones share the same map via
/// an internal `Arc`.  Uses `parking_lot::RwLock` which cannot be poisoned.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Arc<RwLock<HashMap<String, Entry>>>,
    /// Bumped on every successful `register` / `unregister` so that derived
    /// indexes (e.g. the BM25 discovery index) can detect staleness cheaply.
    generation: Arc<AtomicU64>,
}

impl ToolRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool.  Returns `false` if a tool with the same name already exists.
    pub fn register(&self, tool: Arc<dyn Tool>) -> bool {
        let name = tool.spec().name.clone();
        let mut map = self.tools.write();
        if map.contains_key(&name) {
            return false;
        }
        map.insert(name, Entry { tool });
        self.generation.fetch_add(1, Ordering::Release);
        true
    }

    /// Remove a tool by exact name.  Returns `true` if it was registered.
    ///
    /// Used e.g. to hide the tools of a quarantined MCP server.
    pub fn unregister(&self, name: &str) -> bool {
        let removed = self.tools.write().remove(name).is_some();
        if removed {
            self.generation.fetch_add(1, Ordering::Release);
        }
        removed
    }

    /// Monotonic change counter: differs between two calls iff the set of
    /// registered tools changed in between.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Look up a tool by exact name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.read().get(name).map(|e| Arc::clone(&e.tool))
    }

    /// Return all registered tool specs.
    #[must_use]
    pub fn list(&self) -> Vec<ToolSpec> {
        self.tools
            .read()
            .values()
            .map(|e| e.tool.spec().clone())
            .collect()
    }

    /// Return the top-`k` tools whose name or description contains `query` as a
    /// case-insensitive substring.
    ///
    /// Scoring: name match = 2 points, description match = 1 point.
    /// Results are sorted descending by score, then alphabetically by name.
    ///
    /// A BM25 implementation will replace this stub in `agent-router`.
    #[must_use]
    pub fn search(&self, query: &str, k: usize) -> Vec<ToolSpec> {
        if query.is_empty() || k == 0 {
            return Vec::new();
        }
        let q = query.to_lowercase();
        let map = self.tools.read();

        let mut scored: Vec<(u32, &ToolSpec)> = map
            .values()
            .filter_map(|e| {
                let spec = e.tool.spec();
                let name_hit = u32::from(spec.name.to_lowercase().contains(&q));
                let desc_hit = u32::from(spec.description.to_lowercase().contains(&q));
                let score = name_hit * 2 + desc_hit;
                if score > 0 { Some((score, spec)) } else { None }
            })
            .collect();

        scored.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then(a.name.cmp(&b.name)));
        scored.into_iter().take(k).map(|(_, s)| s.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        error::ToolError,
        spec::{Effect, PermissionCategory, Platform},
        tool::{Tool, ToolContext, ToolOutput},
    };
    use async_trait::async_trait;
    use serde_json::{Value, json};

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn spec(&self) -> &ToolSpec {
            static SPEC: std::sync::OnceLock<ToolSpec> = std::sync::OnceLock::new();
            SPEC.get_or_init(|| ToolSpec {
                name: "echo".into(),
                description: "Echo the input text back.".into(),
                input_schema: json!({ "type": "object" }),
                effect: Effect::Read,
                category: PermissionCategory::Script,
                platforms: vec![Platform::All],
            })
        }

        async fn call(&self, _ctx: &ToolContext, input: Value) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text(input.to_string()))
        }
    }

    struct FileTool;

    #[async_trait]
    impl Tool for FileTool {
        fn spec(&self) -> &ToolSpec {
            static SPEC: std::sync::OnceLock<ToolSpec> = std::sync::OnceLock::new();
            SPEC.get_or_init(|| ToolSpec {
                name: "fs.read_file".into(),
                description: "Read bytes from a file on disk.".into(),
                input_schema: json!({ "type": "object" }),
                effect: Effect::Read,
                category: PermissionCategory::Fs,
                platforms: vec![Platform::All],
            })
        }

        async fn call(&self, _ctx: &ToolContext, _input: Value) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text(""))
        }
    }

    #[test]
    fn register_and_get() {
        let reg = ToolRegistry::new();
        assert!(reg.register(Arc::new(EchoTool)));
        assert!(reg.get("echo").is_some());
        assert!(reg.get("missing").is_none());
    }

    #[test]
    fn duplicate_registration_returns_false() {
        let reg = ToolRegistry::new();
        assert!(reg.register(Arc::new(EchoTool)));
        assert!(!reg.register(Arc::new(EchoTool)));
    }

    #[test]
    fn list_returns_all_specs() {
        let reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(FileTool));
        assert_eq!(reg.list().len(), 2);
    }

    #[test]
    fn search_finds_by_name_substring() {
        let reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(FileTool));
        let results = reg.search("echo", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "echo");
    }

    #[test]
    fn search_finds_by_description_substring() {
        let reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(FileTool));
        let results = reg.search("bytes", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "fs.read_file");
    }

    #[test]
    fn search_respects_k_limit() {
        let reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(FileTool));
        let results = reg.search("e", 1);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn search_empty_query_returns_nothing() {
        let reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        assert!(reg.search("", 10).is_empty());
    }

    #[test]
    fn unregister_removes_and_bumps_generation() {
        let reg = ToolRegistry::new();
        let g0 = reg.generation();
        reg.register(Arc::new(EchoTool));
        let g1 = reg.generation();
        assert_ne!(g0, g1);
        assert!(reg.unregister("echo"));
        assert!(reg.get("echo").is_none());
        assert_ne!(g1, reg.generation());
        // Removing a missing tool is a no-op and does not bump the generation.
        let g2 = reg.generation();
        assert!(!reg.unregister("echo"));
        assert_eq!(g2, reg.generation());
    }
}
