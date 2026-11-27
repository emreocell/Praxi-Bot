//! Who is offered the bot management tools (`bot_create`, `bot_update`, `bot_delete`, `bot_list`).
//!
//! Two kinds of run:
//!
//! * a bot with the "Can create bots" switch (`tools.manage_bots`) has them in every run, with a
//!   card for every change (they are always-ask tools);
//! * the owner's OWN chat turn on a bot at the Full level has them too, so "create a bot named
//!   Kralinho" makes a real Praxi bot. There the owner's request is the confirmation: nothing
//!   asks but money (see `Shared::full_bots_waive`).
//!
//! Never to a schedule, a mission session, a channel or webhook run, an external agent, a
//! subagent or a helper bot: those are text no owner typed (or a run that acts for another bot).
//! And never once the run holds untrusted content (a page, a mail, a download, another bot's
//! answer): a text the owner did not write could steer a bot change, which is standing
//! authority. The tools are then not in the list and a call is refused, so only the switch
//! (with its card) is left. The host's ceiling (never broader than the caller, never `full`,
//! only bots it made) is the same for both and is enforced in the daemon, which asks this
//! module about the run again on every call.

use builtin_tools::BOT_TOOL_NAMES;
use protocol::InitiatorKind;

use crate::shared::{RunCtx, Shared};

/// The bot tools the model gets in its direct tool list in an owner's Full chat (the other, the
/// delete, is found with the tool search).
pub(crate) const DIRECT_BOT_TOOLS: [&str; 3] = ["bot_list", "bot_create", "bot_update"];

/// Whether `name` is one of the bot management tools.
pub(crate) fn is_bot_tool(name: &str) -> bool {
    BOT_TOOL_NAMES.contains(&name)
}

impl Shared {
    /// This run is the owner's own chat turn on a bot at the Full level: typed in the app (or the
    /// owner's paired Telegram chat), every message of the run theirs, a top-level run that has
    /// read nothing untrusted. A channel, webhook or external user's message is never the
    /// owner's ([`Shared::full_owner_started`]).
    pub(crate) fn bot_tools_offered(&self, ctx: &RunCtx) -> bool {
        ctx.initiator == InitiatorKind::Human
            && ctx.mission.is_none()
            && ctx.parent_run_id.is_none()
            && ctx.delegation().is_none()
            && ctx.profile.permissions.mode() == permission_engine::AutonomyMode::Full
            && self.full_owner_started(ctx)
            && !ctx.taint.lock().exposure.untrusted
    }

    /// A bot tool call the Full level let through without a card, as the loop asks right before
    /// it runs the call: it freezes the preview itself
    /// ([`tool_registry::ToolContext::full_waived_preview`]). The same run test as the offer.
    pub(crate) fn bot_preview_waived(&self, ctx: &RunCtx, name: &str) -> bool {
        is_bot_tool(name) && self.bot_tools_offered(ctx)
    }

    /// Whether this run may see and call `name`: every tool but the bot tools always; those
    /// when the bot has the switch or this is the owner's Full chat turn.
    pub(crate) fn bot_tool_allowed(&self, ctx: &RunCtx, name: &str) -> bool {
        !is_bot_tool(name) || ctx.profile.tools.manage_bots || self.bot_tools_offered(ctx)
    }
}
