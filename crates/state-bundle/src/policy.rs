//! Versioned global policies and conservative permission-ceiling comparison.

use std::{fs, io::Read, path::Path};

pub(crate) use daemon_api::policy::POLICY_FILE;
use permission_engine::{Level, Policy, Rule};
use serde::Serialize;
use serde_json::Value;

use crate::{ImportError, Result, invalid};

/// Schema of the required `policy.json` payload in a state bundle.
pub const POLICY_SCHEMA_VERSION: u32 = 1;
pub(crate) const MAX_POLICY_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Serialize)]
struct VersionedPolicy<'a> {
    schema_version: u32,
    // The daemon consumes `categories` at the top level. Keeping that shape
    // lets its canonical loader read the installed file without conversion.
    #[serde(flatten)]
    policy: &'a Policy,
}

pub(crate) fn export(data: &Path) -> Result<Vec<u8>> {
    // A genuinely absent source policy has the daemon's first-run default.
    // Materialize it so later imports never infer a policy from an omission.
    let policy = load_existing(data, false)?.unwrap_or_else(Policy::approve_destructive);
    let bytes = serde_json::to_vec(&VersionedPolicy {
        schema_version: POLICY_SCHEMA_VERSION,
        policy: &policy,
    })?;
    if bytes.len() as u64 > MAX_POLICY_BYTES {
        return Err(ImportError::SizeLimit);
    }
    Ok(bytes)
}

pub(crate) fn imported(staging: &Path) -> Result<Policy> {
    load_existing(staging, true)?.ok_or_else(|| invalid("missing policy.json"))
}

pub(crate) fn check_destination(
    data: &Path,
    incoming: &Policy,
    allow_relaxation: bool,
) -> Result<()> {
    // Even an approved relaxation must not treat an unreadable/malformed
    // destination policy as absent. Its current permission ceiling is unknown.
    // A missing file is not an open ceiling either. The daemon's effective
    // policy is `approve_destructive` (Ask everywhere), on a fresh empty target
    // as well as on an initialized host. Loosening that default needs approval.
    let current = load_existing(data, false)?.unwrap_or_else(Policy::approve_destructive);
    if !allow_relaxation && !preserves_ceiling(incoming, &current) {
        return Err(ImportError::PolicyRelaxation);
    }
    Ok(())
}

fn load_existing(data: &Path, require_version: bool) -> Result<Option<Policy>> {
    let path = data.join(POLICY_FILE);
    match fs::symlink_metadata(&path) {
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(invalid("policy.json must be a regular file"));
        }
        Ok(meta) if meta.len() > MAX_POLICY_BYTES => return Err(ImportError::SizeLimit),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let mut text = String::new();
    // Bound the read as well as the metadata check. This temporary owns the
    // handle and closes it before validation, replacement, or staging cleanup.
    fs::File::open(path)?
        .take(MAX_POLICY_BYTES + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_POLICY_BYTES {
        return Err(ImportError::SizeLimit);
    }
    parse(&text, require_version).map(Some)
}

fn parse(text: &str, require_version: bool) -> Result<Policy> {
    // Use the daemon's validator, rather than a second policy deserializer.
    let policy = daemon_api::validate_policy(text).map_err(|_| invalid("invalid policy.json"))?;
    let document: Value = serde_json::from_str(text)?;
    match document.get("schema_version") {
        Some(version) if version.as_u64() == Some(u64::from(POLICY_SCHEMA_VERSION)) => {}
        None if !require_version => {}
        _ => return Err(invalid("missing or unsupported policy schema version")),
    }
    // A versioned migration must not silently drop unknown constraints that
    // the runtime's forward-compatible deserializer happens to ignore.
    only_fields(&document, &["schema_version", "categories"])?;
    for rule in document["categories"]
        .as_object()
        .ok_or_else(|| invalid("invalid policy categories"))?
        .values()
    {
        only_fields(rule, &["level", "by_initiator"])?;
        known_level_fields(&rule["level"])?;
        if let Some(overrides) = rule.get("by_initiator") {
            for level in overrides
                .as_object()
                .ok_or_else(|| invalid("invalid policy initiators"))?
                .values()
            {
                known_level_fields(level)?;
            }
        }
    }
    Ok(policy)
}

fn only_fields(value: &Value, fields: &[&str]) -> Result<()> {
    if !value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| fields.contains(&key.as_str())))
    {
        return Err(invalid("unknown policy fields"));
    }
    Ok(())
}

fn known_level_fields(level: &Value) -> Result<()> {
    if level["level"] == "allow_with_limits" {
        only_fields(
            level,
            &["level", "max_per_hour", "max_amount_minor", "path_patterns"],
        )
    } else {
        only_fields(level, &["level"])
    }
}

fn preserves_ceiling(incoming: &Policy, current: &Policy) -> bool {
    let fallback = Rule::default();
    incoming
        .categories
        .keys()
        .chain(current.categories.keys())
        .all(|category| {
            let new = incoming.categories.get(category).unwrap_or(&fallback);
            let old = current.categories.get(category).unwrap_or(&fallback);
            no_more_permissive(&new.level, &old.level)
                && new
                    .by_initiator
                    .keys()
                    .chain(old.by_initiator.keys())
                    .all(|&kind| no_more_permissive(new.level_for(kind), old.level_for(kind)))
        })
}

fn no_more_permissive(incoming: &Level, current: &Level) -> bool {
    // Do not use Level's rank alone: ordinary Ask can be waived by a grant,
    // whereas an unsupported-limit Ask cannot. Keep unequal limit policies
    // incomparable, including Ask <-> AllowWithLimits, until the engine can
    // prove containment of every limit. A reviewed approval can override this.
    incoming == current || matches!(incoming, Level::Deny) || matches!(current, Level::Allow)
}

#[cfg(test)]
mod tests {
    use permission_engine::Limits;
    use protocol::{InitiatorKind, PermissionCategory};

    use super::*;

    fn shell(rule: Rule) -> Policy {
        Policy {
            categories: [(PermissionCategory::Shell, rule)].into(),
        }
    }

    #[test]
    fn comparison_preserves_limits_even_when_a_plain_ask_has_lower_rank() {
        let limited = Level::AllowWithLimits(Limits {
            max_per_hour: Some(1),
            max_amount_minor: Some(100),
            path_patterns: vec!["safe/**".into()],
        });
        let broader = Level::AllowWithLimits(Limits {
            max_per_hour: Some(10),
            max_amount_minor: None,
            path_patterns: vec!["**".into()],
        });
        let levels = [Level::Deny, Level::Ask, limited, broader, Level::Allow];
        let expected = [
            [true, true, true, true, true],
            [false, true, false, false, true],
            [false, false, true, false, true],
            [false, false, false, true, true],
            [false, false, false, false, true],
        ];
        for (i, incoming) in levels.iter().enumerate() {
            for (j, current) in levels.iter().enumerate() {
                assert_eq!(
                    preserves_ceiling(
                        &shell(Rule::new(incoming.clone())),
                        &shell(Rule::new(current.clone()))
                    ),
                    expected[i][j],
                    "{incoming:?} replacing {current:?}"
                );
            }
        }
    }

    #[test]
    fn missing_categories_inherit_ask_and_cannot_remove_a_deny() {
        let empty = Policy::default();
        let deny = shell(Rule::new(Level::Deny));
        let ask = shell(Rule::new(Level::Ask));
        assert!(!preserves_ceiling(&empty, &deny));
        assert!(preserves_ceiling(&deny, &empty));
        assert!(preserves_ceiling(&empty, &ask));
        assert!(preserves_ceiling(&ask, &empty));
    }

    #[test]
    fn every_initiator_override_and_schedule_inheritance_are_checked() {
        for kind in [
            InitiatorKind::Human,
            InitiatorKind::Routine,
            InitiatorKind::Channel,
            InitiatorKind::Subagent,
            InitiatorKind::Schedule,
        ] {
            let mut rule = Rule::new(Level::Ask);
            rule.by_initiator.insert(kind, Level::Deny);
            let current = shell(rule);
            assert!(!preserves_ceiling(&shell(Rule::new(Level::Ask)), &current));
            assert!(preserves_ceiling(&current, &shell(Rule::new(Level::Ask))));
        }

        let mut old = Rule::new(Level::Ask);
        old.by_initiator.insert(InitiatorKind::Human, Level::Deny);
        let mut new = old.clone();
        new.by_initiator.insert(InitiatorKind::Schedule, Level::Ask);
        assert!(!preserves_ceiling(&shell(new), &shell(old)));

        // Removing an explicit schedule deny also exposes a human allow.
        let mut old = Rule::new(Level::Ask);
        old.by_initiator.insert(InitiatorKind::Human, Level::Allow);
        old.by_initiator
            .insert(InitiatorKind::Schedule, Level::Deny);
        let mut new = old.clone();
        new.by_initiator.remove(&InitiatorKind::Schedule);
        assert!(!preserves_ceiling(&shell(new), &shell(old)));
    }
}
