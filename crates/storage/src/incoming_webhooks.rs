//! Owner-created incoming webhooks. The signing secret is readable only by the
//! receive path; list and update never return it.

use protocol::{BotId, UserId};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::db::Storage;
use crate::error::Result;

/// One incoming webhook, without its signing secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingWebhook {
    /// Primary key.
    pub id: Uuid,
    /// Owner.
    pub user_id: UserId,
    /// Bot the call starts a run for.
    pub bot_id: BotId,
    /// Name the owner chose.
    pub name: String,
    /// What the owner wants done with the data. The payload is not this text.
    pub instruction: String,
    /// Whether calls are accepted.
    pub enabled: bool,
    /// Creation time.
    pub created_at_ms: i64,
    /// Last edit.
    pub updated_at_ms: i64,
    /// Last accepted call, if any.
    pub last_fired_at_ms: Option<i64>,
}

const SELECT: &str = "SELECT id, user_id, bot_id, name, instruction, enabled, created_at_ms, updated_at_ms, last_fired_at_ms";

fn row_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<IncomingWebhook> {
    let id: String = row.get(0)?;
    let user: String = row.get(1)?;
    let bot: String = row.get(2)?;
    Ok(IncomingWebhook {
        id: Uuid::parse_str(&id).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        user_id: UserId(Uuid::parse_str(&user).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
        })?),
        bot_id: BotId(Uuid::parse_str(&bot).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
        })?),
        name: row.get(3)?,
        instruction: row.get(4)?,
        enabled: row.get::<_, i64>(5)? != 0,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
        last_fired_at_ms: row.get(8)?,
    })
}

impl Storage {
    /// How many webhooks this bot already has.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn count_incoming_webhooks(&self, user_id: UserId, bot_id: BotId) -> Result<i64> {
        Ok(self.conn().query_row(
            "SELECT COUNT(*) FROM incoming_webhooks WHERE user_id = ?1 AND bot_id = ?2",
            params![user_id.to_string(), bot_id.to_string()],
            |r| r.get(0),
        )?)
    }

    /// Insert a webhook. The signing secret lives in the secret store; the
    /// column stays empty (it only holds the plaintext of rows made before the
    /// secret moved, until [`Self::clear_legacy_webhook_secret`] blanks it).
    ///
    /// # Errors
    /// SQLite errors; [`crate::StorageError::Conflict`] while the bot is being deleted.
    pub fn insert_incoming_webhook(&self, hook: &IncomingWebhook) -> Result<()> {
        if self.is_retired(hook.bot_id) {
            return Err(crate::StorageError::Conflict(
                "the bot is being deleted".into(),
            ));
        }
        let secret = "";
        self.conn().execute(
            "INSERT INTO incoming_webhooks (id, user_id, bot_id, name, instruction, secret, enabled, created_at_ms, updated_at_ms, last_fired_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                hook.id.to_string(),
                hook.user_id.to_string(),
                hook.bot_id.to_string(),
                hook.name,
                hook.instruction,
                secret,
                i64::from(hook.enabled),
                hook.created_at_ms,
                hook.updated_at_ms,
                hook.last_fired_at_ms,
            ],
        )?;
        Ok(())
    }

    /// Insert a webhook the way builds before the secret store did (plaintext
    /// in the column). Only the migration tests use it.
    ///
    /// # Errors
    /// SQLite errors.
    #[doc(hidden)]
    pub fn insert_legacy_incoming_webhook(
        &self,
        hook: &IncomingWebhook,
        secret: &str,
    ) -> Result<()> {
        self.insert_incoming_webhook(hook)?;
        self.conn().execute(
            "UPDATE incoming_webhooks SET secret = ?1 WHERE id = ?2",
            params![secret, hook.id.to_string()],
        )?;
        Ok(())
    }

    /// Webhooks of one bot, oldest first. No secrets.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn list_incoming_webhooks(
        &self,
        user_id: UserId,
        bot_id: BotId,
    ) -> Result<Vec<IncomingWebhook>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "{SELECT} FROM incoming_webhooks WHERE user_id = ?1 AND bot_id = ?2 ORDER BY created_at_ms, id"
        ))?;
        let rows = st.query_map(params![user_id.to_string(), bot_id.to_string()], row_of)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// One webhook of this user, without the secret.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn get_incoming_webhook(
        &self,
        user_id: UserId,
        id: Uuid,
    ) -> Result<Option<IncomingWebhook>> {
        Ok(self
            .conn()
            .query_row(
                &format!("{SELECT} FROM incoming_webhooks WHERE user_id = ?1 AND id = ?2"),
                params![user_id.to_string(), id.to_string()],
                row_of,
            )
            .optional()?)
    }

    /// Plaintext signing secrets still in the database (rows made before the
    /// secret store held them). Empty once every row is migrated.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn legacy_webhook_secrets(&self) -> Result<Vec<(Uuid, String)>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT id, secret FROM incoming_webhooks WHERE secret <> ''")?;
        let rows = st.query_map([], |row| {
            let id: String = row.get(0)?;
            let id = Uuid::parse_str(&id).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok((id, row.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Blank the plaintext column of one webhook (its secret now lives in the
    /// secret store).
    ///
    /// # Errors
    /// SQLite errors.
    pub fn clear_legacy_webhook_secret(&self, id: Uuid) -> Result<()> {
        self.conn().execute(
            "UPDATE incoming_webhooks SET secret = '' WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    /// One webhook by id, with the legacy plaintext column (empty once the
    /// secret is in the secret store). Callers must not log or return it.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn incoming_webhook_secret(&self, id: Uuid) -> Result<Option<(IncomingWebhook, String)>> {
        Ok(self
            .conn()
            .query_row(
                &format!("{SELECT}, secret FROM incoming_webhooks WHERE id = ?1"),
                params![id.to_string()],
                |row| {
                    let hook = row_of(row)?;
                    let secret: String = row.get(9)?;
                    Ok((hook, secret))
                },
            )
            .optional()?)
    }

    /// Rename, rewrite the instruction, or enable / disable. No secret change.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn update_incoming_webhook(&self, hook: &IncomingWebhook) -> Result<bool> {
        let n = self.conn().execute(
            "UPDATE incoming_webhooks SET name = ?1, instruction = ?2, enabled = ?3, updated_at_ms = ?4 WHERE id = ?5 AND user_id = ?6",
            params![
                hook.name,
                hook.instruction,
                i64::from(hook.enabled),
                hook.updated_at_ms,
                hook.id.to_string(),
                hook.user_id.to_string(),
            ],
        )?;
        Ok(n == 1)
    }

    /// Record that a call was accepted.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn mark_incoming_webhook_fired(&self, id: Uuid, at_ms: i64) -> Result<()> {
        self.conn().execute(
            "UPDATE incoming_webhooks SET last_fired_at_ms = ?1 WHERE id = ?2",
            params![at_ms, id.to_string()],
        )?;
        Ok(())
    }

    /// Delete one webhook of this user.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn delete_incoming_webhook(&self, user_id: UserId, id: Uuid) -> Result<bool> {
        let n = self.conn().execute(
            "DELETE FROM incoming_webhooks WHERE user_id = ?1 AND id = ?2",
            params![user_id.to_string(), id.to_string()],
        )?;
        Ok(n == 1)
    }

    /// Remember an accepted signature until `expires_ms`. `false` means it was
    /// already remembered (a replay) or `max` live entries exist. Expired
    /// entries are dropped first.
    ///
    /// # Errors
    /// SQLite errors.
    pub fn remember_webhook_signature(
        &self,
        id: Uuid,
        signature: &str,
        now_ms: i64,
        expires_ms: i64,
        max: i64,
    ) -> Result<bool> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM incoming_webhook_seen WHERE expires_ms <= ?1",
            params![now_ms],
        )?;
        let live: i64 = conn.query_row("SELECT COUNT(*) FROM incoming_webhook_seen", [], |r| {
            r.get(0)
        })?;
        if live >= max {
            return Ok(false);
        }
        let n = conn.execute(
            "INSERT OR IGNORE INTO incoming_webhook_seen (webhook_id, signature, expires_ms) VALUES (?1, ?2, ?3)",
            params![id.to_string(), signature, expires_ms],
        )?;
        Ok(n == 1)
    }

    /// Forget a signature again (the run it was for could not start).
    ///
    /// # Errors
    /// SQLite errors.
    pub fn forget_webhook_signature(&self, id: Uuid, signature: &str) -> Result<()> {
        self.conn().execute(
            "DELETE FROM incoming_webhook_seen WHERE webhook_id = ?1 AND signature = ?2",
            params![id.to_string(), signature],
        )?;
        Ok(())
    }
}
