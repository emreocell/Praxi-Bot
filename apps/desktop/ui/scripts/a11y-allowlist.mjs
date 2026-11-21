// Known accessibility debt for `pnpm check:a11y`.  One entry per (rule, selector); every entry needs a
// comment saying why it is allowed.  The check fails on any violation NOT listed here, and ALSO on an
// entry that no longer matches anything (so fixing a problem forces you to delete its line).
//   { rule: 'axe-rule-id', selector: 'substring of the axe target', screens: ['optional', 'screen names'], reason }
export const allowlist = [
  // The chat composer is a textarea that acts as a mention/command combobox.  `role="combobox"` on a
  // <textarea> is not allowed by ARIA-in-HTML, but screen-reader behaviour of the replacement
  // (a contenteditable or a wrapper pattern) needs the manual pass of #171 first.  Minor severity.
  { rule: 'aria-allowed-role', selector: 'textarea', screens: ['chat-approval'], reason: 'composer combobox on a textarea; needs manual screen-reader pass (#171) before changing the pattern' },
  // The bot panel's small section headings use --color-text-muted on the panel surface and miss 4.5:1.
  // Fixing it means changing the muted token (or the panel surface) for every theme, which needs a
  // design decision plus a check:themes run, so it is recorded here instead of changed ad hoc.
  { rule: 'color-contrast', selector: '#bot-panel-computer', screens: ['chat-right-panel'], reason: 'muted section label on the panel surface; token change needs design sign-off' },
  { rule: 'color-contrast', selector: '#bot-panel-upcoming', screens: ['chat-right-panel'], reason: 'muted section label on the panel surface; token change needs design sign-off' },
  { rule: 'color-contrast', selector: '._label_wynoi_2', screens: ['chat-right-panel'], reason: 'muted "Quota left" label (QuotaRings) on the panel surface; token change needs design sign-off' }
];
