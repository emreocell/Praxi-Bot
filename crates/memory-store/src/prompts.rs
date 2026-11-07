//! Prompt templates (shipped as files under `prompts/`).

/// Extraction prompt; placeholder `{{excerpt}}`.
pub const EXTRACT: &str = include_str!("../prompts/extract.md");
/// Consolidation prompt; placeholders `{{candidate}}`, `{{similar}}`.
pub const CONSOLIDATE: &str = include_str!("../prompts/consolidate.md");

/// Fill the extraction prompt.
#[must_use]
pub fn render_extract(excerpt: &str) -> String {
    EXTRACT.replace("{{excerpt}}", excerpt)
}

/// Fill the consolidation prompt.
#[must_use]
pub fn render_consolidate(candidate: &str, similar: &str) -> String {
    CONSOLIDATE
        .replace("{{candidate}}", candidate)
        .replace("{{similar}}", similar)
}
