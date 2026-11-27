// Known accessibility debt for `pnpm check:a11y`.  One entry per (rule, selector); every entry needs a
// comment saying why it is allowed.  The check fails on any violation NOT listed here, and ALSO on an
// entry that no longer matches anything (so fixing a problem forces you to delete its line).
//   { rule: 'axe-rule-id', selector: 'substring of the axe target', screens: ['optional', 'screen names'], reason }
export const allowlist = [
  {
    rule: 'target-size',
    selector: 'input[data-testid="mcp-trust-custom-no"]',
    screens: ['apps-custom-mcp'],
    reason: 'McpTrustChoice uses compact radio inputs; touch target sizing tracked with Apps #1002 follow-up.'
  },
  {
    rule: 'target-size',
    selector: 'input[data-testid="mcp-trust-custom-yes"]',
    screens: ['apps-custom-mcp'],
    reason: 'Same as mcp-trust-custom-no (custom MCP form trust radios).'
  },
];