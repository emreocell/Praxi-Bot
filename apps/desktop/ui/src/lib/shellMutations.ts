/**
 * Single-regression mutations of the real shell CSS / policy.  Each one, applied ALONE,
 * must make the layout check fail; the self-test in `check-layout.mjs` and the unit tests run them all.
 */
import type { MatrixOptions } from "./shellGeometry.ts";

export interface Mutation {
  name: string;
  css?: (css: string) => string;
  options?: MatrixOptions;
  /** Matches the joined contract + geometry failures this mutation must cause. */
  expect: RegExp;
}

function replaceOnce(css: string, from: string | RegExp, to: string): string {
  const next = css.replace(from, to);
  if (next === css) throw new Error(`mutation target not found: ${String(from)}`);
  return next;
}

const OPEN_RULE = '.panel[data-open="true"] {';
/** Append a declaration at the END of the float-open rule (so it wins over the earlier shorthand). */
const appendToFloatOpen = (css: string, decl: string): string =>
  replaceOnce(css, /(:global\(:root\[data-float="on"\]\) \.panel\[data-open="true"\] \{[^}]*)\}/, `$1  ${decl}
}`);

export const MUTATIONS: Mutation[] = [
  {
    name: "later duplicate .panelInner { width: var(--panel-w) } (old clipping width wins the real cascade)",
    css: (css) => `${css}\n.panelInner { width: var(--panel-w); }\n`,
    expect: /\.panelInner \{ width \}.*declared once/
  },
  {
    name: "open panel width calc(var(--panel-w) + 80px)",
    css: (css) => replaceOnce(css, /(\.panel\[data-open="true"\] \{\s*width:) var\(--panel-w\);/, "$1 calc(var(--panel-w) + 80px);"),
    expect: /\.panel \(plainOpen\) width/
  },
  {
    name: "float-open panel gets border-right: 12px (longhand, not the border shorthand)",
    css: (css) => appendToFloatOpen(css, "border-right: 12px solid var(--color-border);"),
    expect: /clipped \d+px|panelInner width/
  },
  {
    name: "float-open panel border-right-width longhand 9px",
    css: (css) => appendToFloatOpen(css, "border-right-width: 9px;"),
    expect: /clipped \d+px|panelInner width/
  },
  {
    name: "inner width 0px (content column collapses)",
    css: (css) => replaceOnce(css, /(\.panelInner \{\s*width:) calc\([^;]*\);/, "$1 0px;"),
    expect: /panelInner width 0 != available content width/
  },
  {
    name: "extra rule overriding the open panel in a media query",
    css: (css) => `${css}\n@media (min-width: 1px) { ${OPEN_RULE} width: 10px; } }\n`,
    expect: /at-rule .*Sidebar\.module\.css:\d+|Sidebar\.module\.css:\d+ :: at-rule/
  },
  {
    name: "unreviewed selector overriding .panelInner geometry",
    css: (css) => `${css}\n:global(:root[data-style="glass"]) .panelInner { width: 0px; }\n`,
    expect: /extra override rejected/
  },
  {
    name: "window-resize transition guard removed",
    css: (css) => replaceOnce(css, ":global(:root[data-window-resizing]) .panel {", ":global(:root[data-never]) .panel {"),
    expect: /data-window-resizing/
  },
  {
    name: "rail grows back at once when the panel starts closing",
    options: { railPolicy: "immediate" },
    expect: /closing t=\d+ms progress=.*\(transition\): main [\d.]+px < (280|MAIN_MIN)/
  }
];
