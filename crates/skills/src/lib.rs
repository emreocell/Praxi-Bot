//! Markdown skill system.
//!
//! A skill is a directory with a `SKILL.md` (frontmatter + markdown body) and
//! optional extra files. The agent sees only a short index
//! ([`SkillIndex::prompt_block`]) and loads full content on demand through the
//! `skill_*` tools ([`tools`]).
//!
//! Sources, later overriding earlier by `name`: bundled (repo `skills/`),
//! shared (`<data_dir>/skills/`), per-bot (`<data_dir>/bots/<id>/skills/`).
//!
//! Third-party skills are installed, scanned, locked and updated through
//! [`hub::SkillHub`]. The static scan is advisory. Installing or updating a
//! git or index skill requires an accept token bound to that preview's
//! fingerprint; a local folder needs the same token unless it is low risk and
//! `trust_local` is set.
//!
//! # Wiring
//! ```ignore
//! let index = skills::SkillIndex::load_roots(skills::SkillRoots::from_data_dir(&data_dir));
//! for tool in skills::tools(&index) { registry.register(tool); }
//! // Tools resolve ToolContext.bot_id. Prompt callers must pass it explicitly:
//! let block = index.prompt_block_for_bot(Some(bot_id), &context, &suggested)?;
//! system_prompt.push_str(&block);
//! ```
//!
//! The agent-core caller passes the run's bot id to `prompt_block_for_bot`, so
//! a bot's learned skills are suggested in its own prompt and no other bot's.

mod bundled;
mod frontmatter;
pub mod hub;
mod index;
mod scoped_store;
mod skill;
mod tools;

pub use index::{
    MAX_FILE_BYTES, PROMPT_BUDGET_BYTES, SKILL_FILE, SkillIndex, SkillIssue, SkillPaths,
    SkillRoots, validate_bot_id,
};
pub use scoped_store::WrittenSkill;
pub use skill::{
    MAX_DESCRIPTION_LEN, MAX_NAME_LEN, MAX_SKILL_BYTES, Skill, SkillContext, Source, When,
    parse_skill, validate_name,
};
pub use tools::{SKILL_TOOL_NAMES, tools};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod scopes_tests;
