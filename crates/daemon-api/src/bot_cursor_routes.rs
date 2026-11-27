//! The owner's switch for the bot's own pointer (`/v1/bot-cursor`).
//!
//! While a bot uses the computer it draws a pointer of its own (a transparent window over the
//! desktop, an overlay in the managed browser's page, a marker in the live viewer) so the
//! owner can see where it is about to click. This is the one switch for all three, on by
//! default. It lives in `<data dir>/bot-cursor.json`, is read at start and is owner-only; it
//! only changes what is drawn, never what a bot is allowed to do.

use std::path::{Path, PathBuf};

use axum::{Json, extract::State, http::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::{ApiError, AppState, error::ErrorBody, routes::ApiJson};

const FILE: &str = "bot-cursor.json";

/// Whether the bot's own pointer is drawn.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BotCursorSettings {
    /// `false`: no bot pointer on the desktop, in the browser page or in the live viewer.
    pub enabled: bool,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

/// Read the stored switch and apply it to this process. A missing or unreadable file keeps the
/// default (on): the pointer is a courtesy to the owner, not a safety control.
pub(crate) fn load(data_dir: &Path) {
    if let Some(s) = std::fs::read(path(data_dir))
        .ok()
        .and_then(|b| serde_json::from_slice::<BotCursorSettings>(&b).ok())
    {
        live_view::bot_cursor::set_enabled(s.enabled);
    }
}

fn store(data_dir: &Path, settings: &BotCursorSettings) -> std::io::Result<()> {
    let tmp = path(data_dir).with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(settings)?)?;
    std::fs::rename(&tmp, path(data_dir))
}

/// Whether the bot's own pointer is drawn.
#[utoipa::path(get, path = "/v1/bot-cursor", tag = "computer", security(("bearer" = [])),
    responses((status = 200, body = BotCursorSettings)))]
pub(crate) async fn get_bot_cursor() -> Json<BotCursorSettings> {
    Json(BotCursorSettings {
        enabled: live_view::bot_cursor::enabled(),
    })
}

/// Turn the bot's own pointer on or off (owner only). Off removes it everywhere at once.
#[utoipa::path(put, path = "/v1/bot-cursor", tag = "computer", security(("bearer" = [])),
    request_body = BotCursorSettings,
    responses((status = 200, body = BotCursorSettings), (status = 403, body = ErrorBody)))]
pub(crate) async fn put_bot_cursor(
    State(s): State<AppState>,
    headers: HeaderMap,
    ApiJson(input): ApiJson<BotCursorSettings>,
) -> Result<Json<BotCursorSettings>, ApiError> {
    s.require_owner(&headers)?;
    store(s.storage().data_dir(), &input).map_err(ApiError::internal)?;
    live_view::bot_cursor::set_enabled(input.enabled);
    let _ = s.storage().append_audit(
        s.user_id(),
        None,
        "user",
        "bot_cursor.settings",
        &json!({"enabled": input.enabled}),
    );
    Ok(Json(input))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_is_stored_and_read_back_and_a_bad_file_keeps_the_default() {
        let dir = tempfile::tempdir().unwrap();
        // No file: untouched (on).
        load(dir.path());
        assert!(live_view::bot_cursor::enabled());
        store(dir.path(), &BotCursorSettings { enabled: false }).unwrap();
        load(dir.path());
        assert!(!live_view::bot_cursor::enabled());
        std::fs::write(path(dir.path()), b"not json").unwrap();
        store(dir.path(), &BotCursorSettings { enabled: true }).unwrap();
        load(dir.path());
        assert!(live_view::bot_cursor::enabled());
        std::fs::write(path(dir.path()), b"{\"enabled\":1,\"x\":2}").unwrap();
        load(dir.path());
        assert!(
            live_view::bot_cursor::enabled(),
            "a bad file changes nothing"
        );
    }
}
