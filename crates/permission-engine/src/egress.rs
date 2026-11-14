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

/// Whether an egress call must ask because of [`ContentExposure`].
///
/// * Clean runs are unchanged.
/// * Untrusted content only: `configured_search` (a `web_search` that can only
///   reach the configured search provider, never an arbitrary URL) stays
///   prompt-free. Every other egress asks.
/// * A private read: every egress asks.
/// * `explicit_waiver` is the per-bot "allow egress after reads" setting. It
///   is never implied by a tool or category grant.
///
/// A tool that is not egress is never forced by this rule.
#[must_use]
pub const fn egress_must_ask(
    exposure: ContentExposure,
    egress: bool,
    configured_search: bool,
    explicit_waiver: bool,
) -> bool {
    if explicit_waiver || !egress || !exposure.is_tainted() {
        return false;
    }
    if exposure.has_private() {
        return true;
    }
    // Untrusted only.
    !configured_search
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_runs_do_not_ask() {
        let clean = ContentExposure::clean();
        assert!(!egress_must_ask(clean, true, false, false));
        assert!(!egress_must_ask(clean, true, true, false));
        assert!(!egress_must_ask(clean, false, false, false));
    }

    #[test]
    fn untrusted_only_lets_configured_search_through_and_asks_for_the_rest() {
        let mut exposure = ContentExposure::clean();
        exposure.observe(true, false);
        assert!(exposure.untrusted_only());
        assert!(!egress_must_ask(exposure, true, true, false));
        assert!(egress_must_ask(exposure, true, false, false));
        assert!(!egress_must_ask(exposure, false, false, false));
    }

    #[test]
    fn private_read_asks_for_every_egress_including_search() {
        let mut exposure = ContentExposure::clean();
        exposure.observe(false, true);
        assert!(exposure.has_private());
        assert!(egress_must_ask(exposure, true, true, false));
        assert!(egress_must_ask(exposure, true, false, false));
        exposure.observe(true, false);
        assert!(exposure.has_private());
        assert!(egress_must_ask(exposure, true, true, false));
    }

    #[test]
    fn explicit_waiver_is_the_only_thing_that_skips_the_gate() {
        let mut exposure = ContentExposure::clean();
        exposure.observe(true, true);
        assert!(!egress_must_ask(exposure, true, false, true));
        assert!(egress_must_ask(exposure, true, false, false));
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
