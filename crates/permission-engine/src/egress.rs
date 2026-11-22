//! Egress gate: once a run has read untrusted or private content, sending data
//! off the machine is no longer covered by a read shortcut or a standing grant.
//!
//! Deny and the absolute floors are applied by the caller before this rule.
//! This function only answers whether the egress gate itself requires an ask.

/// What a run has already put into the model's context.
///
/// Bits are only ever set, never cleared, for the life of the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContentExposure {
    /// A tool result (or recalled memory) came from an untrusted source.
    pub untrusted: bool,
    /// A tool result (or recalled memory) came from a private source.
    pub private: bool,
}

impl ContentExposure {
    /// Nothing sensitive has entered the context.
    #[must_use]
    pub const fn clean() -> Self {
        Self {
            untrusted: false,
            private: false,
        }
    }

    /// Record that content of this kind entered the context. Never clears a bit.
    pub fn observe(&mut self, untrusted: bool, private_read: bool) {
        self.untrusted |= untrusted;
        self.private |= private_read;
    }

    /// Union with exposure the run inherited (parent run, stored history).
    pub fn inherit(&mut self, other: Self) {
        self.observe(other.untrusted, other.private);
    }

    /// Private content is present, whether or not untrusted content is too.
    #[must_use]
    pub const fn has_private(self) -> bool {
        self.private
    }

    /// Only untrusted content, no private read.
    #[must_use]
    pub const fn untrusted_only(self) -> bool {
        self.untrusted && !self.private
    }

    /// Either kind of sensitive content is in context.
    #[must_use]
    pub const fn is_tainted(self) -> bool {
        self.untrusted || self.private
    }
}

/// Where an egress call can send data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressReach {
    /// The call can be aimed at an arbitrary host.
    Arbitrary,
    /// A `web_search` that can only reach the configured search provider.
    ConfiguredSearch,
    /// An action inside the browser the OWNER connected as a browser (an MCP
    /// server of kind `browser` or the built-in Playwright preset): navigate,
    /// snapshot, click, type, evaluate inside the page. It is the browser's own
    /// page traffic, not a program start and not a file upload (those tools
    /// are classified apart and always ask).
    OwnerBrowser,
}

/// Whether an egress call must ask because of [`ContentExposure`].
///
/// * Clean runs are unchanged.
/// * A PRIVATE read alone (the user's own files, screen, window list, memory) never
///   asks here: with no untrusted text in the context there is nothing that can
///   instruct the model to send it anywhere. The tools that always ask (sends,
///   money, never-list) are decided before this rule.
/// * Untrusted content only: a configured search and an action inside the
///   owner's connected browser stay prompt-free (there is no private data in
///   the context they could carry). Every other egress asks.
/// * Untrusted AND private content: every egress asks, search and browser
///   actions included (a query, a URL or a page script could carry the
///   private data out).
/// * `explicit_waiver` is the per-bot "allow egress after reads" setting. It
///   is never implied by a tool or category grant.
///
/// A tool that is not egress is never forced by this rule.
#[must_use]
pub const fn egress_must_ask(
    exposure: ContentExposure,
    egress: bool,
    reach: EgressReach,
    explicit_waiver: bool,
) -> bool {
    if explicit_waiver || !egress || !exposure.untrusted {
        return false;
    }
    if exposure.has_private() {
        return true;
    }
    // Untrusted only.
    matches!(reach, EgressReach::Arbitrary)
}

/// Whether a call that starts an arbitrary program must ask because the run
/// read untrusted content.
///
/// Only the `untrusted` bit counts: a program is steered by instructions, and
/// a run that has only read the user's own private files holds none from an
/// attacker. A standing grant, the read shortcut, a Full preset, and the
/// egress waiver do not satisfy it. The waiver allows sending data out; it is
/// not consent to run a command a page asked for.
#[must_use]
pub const fn executes_must_ask(exposure: ContentExposure, executes: bool) -> bool {
    executes && exposure.untrusted
}

#[cfg(test)]
mod tests {
    use super::*;
    use EgressReach::{Arbitrary, ConfiguredSearch, OwnerBrowser};

    #[test]
    fn clean_runs_do_not_ask() {
        let clean = ContentExposure::clean();
        assert!(!egress_must_ask(clean, true, Arbitrary, false));
        assert!(!egress_must_ask(clean, true, ConfiguredSearch, false));
        assert!(!egress_must_ask(clean, false, Arbitrary, false));
    }

    #[test]
    fn untrusted_only_lets_configured_search_through_and_asks_for_the_rest() {
        let mut exposure = ContentExposure::clean();
        exposure.observe(true, false);
        assert!(exposure.untrusted_only());
        assert!(!egress_must_ask(exposure, true, ConfiguredSearch, false));
        assert!(egress_must_ask(exposure, true, Arbitrary, false));
        assert!(!egress_must_ask(exposure, false, Arbitrary, false));
    }

    #[test]
    fn a_private_read_alone_never_asks_but_untrusted_plus_private_asks_for_every_egress() {
        let mut exposure = ContentExposure::clean();
        exposure.observe(false, true);
        assert!(exposure.has_private());
        assert!(!egress_must_ask(exposure, true, ConfiguredSearch, false));
        assert!(!egress_must_ask(exposure, true, Arbitrary, false));
        exposure.observe(true, false);
        assert!(exposure.has_private());
        assert!(egress_must_ask(exposure, true, ConfiguredSearch, false));
    }

    #[test]
    fn an_owner_browser_action_passes_untrusted_only_and_asks_once_private_is_read() {
        let mut exposure = ContentExposure::clean();
        exposure.observe(true, false);
        assert!(!egress_must_ask(exposure, true, OwnerBrowser, false));
        assert!(egress_must_ask(exposure, true, Arbitrary, false));
        exposure.observe(false, true);
        assert!(egress_must_ask(exposure, true, OwnerBrowser, false));
    }

    #[test]
    fn explicit_waiver_is_the_only_thing_that_skips_the_gate() {
        let mut exposure = ContentExposure::clean();
        exposure.observe(true, true);
        assert!(!egress_must_ask(exposure, true, Arbitrary, true));
        assert!(egress_must_ask(exposure, true, Arbitrary, false));
    }

    #[test]
    fn an_executor_asks_only_after_untrusted_content() {
        let clean = ContentExposure::clean();
        assert!(!executes_must_ask(clean, true));
        assert!(!executes_must_ask(clean, false));
        let mut untrusted = ContentExposure::clean();
        untrusted.observe(true, false);
        assert!(executes_must_ask(untrusted, true));
        assert!(!executes_must_ask(untrusted, false));
        let mut both = ContentExposure::clean();
        both.observe(true, true);
        assert!(executes_must_ask(both, true));
        let mut private = ContentExposure::clean();
        private.observe(false, true);
        assert!(!executes_must_ask(private, true));
    }

    #[test]
    fn observe_never_clears() {
        let mut exposure = ContentExposure::clean();
        exposure.observe(true, true);
        exposure.observe(false, false);
        assert!(exposure.untrusted && exposure.private);
        let mut child = ContentExposure::clean();
        child.inherit(exposure);
        assert_eq!(child, exposure);
    }
}
