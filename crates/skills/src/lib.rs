//! Markdown skill system.
//!
//! A skill is a directory with a `SKILL.md` (frontmatter + markdown body) and
//! optional extra files. The agent sees only a short index
//! ([`SkillIndex::prompt_block`]) and loads full content on demand through the
//! `skill_*` tools ([`tools`]).
//!
//! Sources, later overriding earlier by `name`: bundled (repo `skills/`),
//! user (`<data_dir>/skills/`), per-bot (`<data_dir>/bots/<id>/skills/`).
//!
//! # Wiring (agent-core)
//! ```ignore
//! let skills = skills::SkillIndex::load(skills::SkillPaths::from_data_dir(&data_dir, Some(bot_id)));
//! for t in skills::tools(&skills) { registry.register(t); }            // 1. tools
//! system_prompt.push_str(&skills.prompt_block(&skills::SkillContext { // 2. index
//!     domain: page_host, app: foreground_app, text: Some(task_text) })); // 3. per turn
//! ```

mod bundled;
mod frontmatter;
mod index;
mod skill;
mod tools;

pub use index::{
    MAX_FILE_BYTES, PROMPT_BUDGET_BYTES, SKILL_FILE, SkillIndex, SkillIssue, SkillPaths,
};
pub use skill::{
    MAX_DESCRIPTION_LEN, MAX_NAME_LEN, MAX_SKILL_BYTES, Skill, SkillContext, Source, When,
    parse_skill, validate_name,
};
pub use tools::{SKILL_TOOL_NAMES, tools};

#[cfg(test)]
mod tests;
