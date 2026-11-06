//! JSON Schema snapshot tests.
//!
//! Each test generates the schema for a public type and compares it against the
//! committed snapshot in `schema/<TypeName>.json`.  To regenerate all snapshots
//! run:
//!
//! ```text
//! UPDATE_SNAPSHOTS=1 cargo test -p protocol
//! ```

#[cfg(test)]
mod tests {
    use schemars::schema_for;
    use std::{env, fs, path::PathBuf};

    fn schema_dir() -> PathBuf {
        // `CARGO_MANIFEST_DIR` is set by Cargo during test runs.
        let manifest = env!("CARGO_MANIFEST_DIR");
        PathBuf::from(manifest).join("schema")
    }

    fn check_or_update<T: schemars::JsonSchema>(type_name: &str) {
        let schema = schema_for!(T);
        let generated = serde_json::to_string_pretty(&schema).unwrap() + "\n";

        let path = schema_dir().join(format!("{type_name}.json"));
        let update = env::var("UPDATE_SNAPSHOTS").is_ok();

        if update {
            fs::create_dir_all(schema_dir()).unwrap();
            fs::write(&path, &generated).unwrap();
        } else {
            let stored = fs::read_to_string(&path).unwrap_or_else(|_| {
                panic!(
                    "Missing schema snapshot for `{type_name}`. \
                     Run `UPDATE_SNAPSHOTS=1 cargo test -p protocol` to create it."
                )
            });
            assert_eq!(
                stored, generated,
                "Schema for `{type_name}` has changed. \
                 Run `UPDATE_SNAPSHOTS=1 cargo test -p protocol` to update snapshots."
            );
        }
    }

    // Import all public schema-carrying types.
    use crate::{
        envelope::Envelope,
        event::Event,
        ids::{BotId, ChannelRef, ChatId, DeviceId, NodeId, RunId, TaskId, ToolCallId, UserId},
        initiator::{Initiator, InitiatorKind},
        permission::{PermissionCategory, Platform},
    };

    #[test]
    fn snapshot_user_id() {
        check_or_update::<UserId>("UserId");
    }
    #[test]
    fn snapshot_bot_id() {
        check_or_update::<BotId>("BotId");
    }
    #[test]
    fn snapshot_chat_id() {
        check_or_update::<ChatId>("ChatId");
    }
    #[test]
    fn snapshot_run_id() {
        check_or_update::<RunId>("RunId");
    }
    #[test]
    fn snapshot_node_id() {
        check_or_update::<NodeId>("NodeId");
    }
    #[test]
    fn snapshot_device_id() {
        check_or_update::<DeviceId>("DeviceId");
    }
    #[test]
    fn snapshot_task_id() {
        check_or_update::<TaskId>("TaskId");
    }
    #[test]
    fn snapshot_tool_call_id() {
        check_or_update::<ToolCallId>("ToolCallId");
    }
    #[test]
    fn snapshot_channel_ref() {
        check_or_update::<ChannelRef>("ChannelRef");
    }
    #[test]
    fn snapshot_initiator() {
        check_or_update::<Initiator>("Initiator");
    }
    #[test]
    fn snapshot_initiator_kind() {
        check_or_update::<InitiatorKind>("InitiatorKind");
    }
    #[test]
    fn snapshot_permission_category() {
        check_or_update::<PermissionCategory>("PermissionCategory");
    }
    #[test]
    fn snapshot_platform() {
        check_or_update::<Platform>("Platform");
    }
    #[test]
    fn snapshot_event() {
        check_or_update::<Event>("Event");
    }
    #[test]
    fn snapshot_envelope() {
        check_or_update::<Envelope>("Envelope");
    }
}
