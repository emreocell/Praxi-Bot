//! Home Assistant connector as agent tools.
//!
//! - [`tools::LIST`] and [`tools::STATE`] read entity states (`Effect::Read`;
//!   names and attributes are user-editable text and are marked untrusted).
//! - [`tools::CALL`] calls a service such as `light.turn_on`. It acts on the
//!   physical world, so it is `Effect::Write`: it asks at the levels that ask for writes and
//!   runs without a card in Full. Anything that is not a plain device service (locks, alarms,
//!   covers, scripts ...), or that opens, unlocks or disarms something, asks on every level
//!   unless the owner listed it (see [`HomeAssistantClient::needs_confirmation`]), and a fixed
//!   deny list refuses services that run code or stop the hub.
//! - The long-lived access token is a [`secret_store::SecretRef`]. The URL must
//!   be `https`, or plain `http` for a private/loopback address (Home Assistant
//!   on a home network usually has no certificate); redirects are refused and
//!   only the owner's address is ever dialled (see [`client::hub_policy`]).
//! - The hub can be replaced while the daemon runs ([`HomeAssistantConnector::replace`]),
//!   and limited to some bots ([`HomeAssistantConfig::bots`]).

pub mod client;
pub mod tools;

use std::sync::{Arc, PoisonError, RwLock};

use secret_store::SecretStore;
use tool_registry::{Tool, ToolRegistry};
use web_tools::UrlPolicy;

pub use client::{HaError, HomeAssistantClient, HomeAssistantConfig, Probe, hub_policy};
pub use tools::TOOL_NAMES;

struct Live {
    client: Arc<HomeAssistantClient>,
    bots: Option<Vec<String>>,
}

/// The configured hub (none until one is set). Shared by the tools and the settings host, so a
/// change takes effect on the next tool call.
pub struct HomeAssistantConnector {
    live: RwLock<Option<Live>>,
    policy: Arc<dyn UrlPolicy>,
}

impl HomeAssistantConnector {
    /// A connector with no hub, dialling only what `policy` allows.
    #[must_use]
    pub fn new(policy: Arc<dyn UrlPolicy>) -> Self {
        Self {
            live: RwLock::new(None),
            policy,
        }
    }

    /// The address policy (the settings host tests candidate hubs under the same one).
    #[must_use]
    pub fn policy(&self) -> &Arc<dyn UrlPolicy> {
        &self.policy
    }

    /// Use `config` from the next call on (`None`: no hub). Returns why the configuration was
    /// refused; the connector then has no hub.
    pub fn replace(
        &self,
        config: Option<HomeAssistantConfig>,
        store: &Arc<dyn SecretStore>,
    ) -> Option<String> {
        let (live, problem) = match config {
            None => (None, None),
            Some(c) => {
                let bots = c.bots.clone();
                match HomeAssistantClient::with_policy(c, Arc::clone(store), &self.policy) {
                    Ok(client) => (
                        Some(Live {
                            client: Arc::new(client),
                            bots,
                        }),
                        None,
                    ),
                    Err(e) => (None, Some(e.to_string())),
                }
            }
        };
        *self.live.write().unwrap_or_else(PoisonError::into_inner) = live;
        problem
    }

    /// The hub, whoever asks (for the approval rule, which does not know the bot).
    pub(crate) fn client(&self) -> Option<Arc<HomeAssistantClient>> {
        let live = self.live.read().unwrap_or_else(PoisonError::into_inner);
        live.as_ref().map(|l| Arc::clone(&l.client))
    }

    /// The hub, when one is configured and `bot` (`None` = no bot) may use it.
    #[must_use]
    pub fn client_for(&self, bot: Option<&str>) -> Option<Arc<HomeAssistantClient>> {
        let live = self.live.read().unwrap_or_else(PoisonError::into_inner);
        let live = live.as_ref()?;
        let listed = match &live.bots {
            None => true,
            Some(allowed) => bot.is_some_and(|id| allowed.iter().any(|b| b == id)),
        };
        listed.then(|| Arc::clone(&live.client))
    }

    /// Whether a hub is configured at all.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.live
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }
}

/// Register the tools (they stay hidden while no hub is usable); returns how many were added.
#[must_use]
pub fn register(registry: &ToolRegistry, connector: &Arc<HomeAssistantConnector>) -> usize {
    tools::tools(connector)
        .into_iter()
        .filter(|t: &Arc<dyn Tool>| registry.register(t.clone()))
        .count()
}

#[cfg(test)]
mod tests;
