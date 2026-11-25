import { parseDurationMs } from "./shellLayout.ts";

/**
 * CSS contract of the v6 shell: a conservative reader for the rules of
 * `Sidebar.module.css` / `tokens.css` / `global.css` / `theme.ts` that decide the shell's
 * horizontal geometry.  No parser dependency.  It does NOT guess a cascade it cannot follow:
 * every rule that touches a contracted class and sets a geometry property must be one of the
 * reviewed selectors, a property may be declared once per selector, and anything else
 * (at-rules around contracted selectors, `!important`, unsupported properties, extra selectors)
 * is REJECTED with a message naming the rule, file and expected value.  The reviewed rules are
 * then evaluated with a real (specificity-ordered) cascade per panel state, longhands included.
 */

export interface CssSources {
  sidebarCss: string;
  tokensCss: string;
  globalCss: string;
  themeTs: string;
  /** `Sidebar.tsx`: where `--panel-w` is set. */
  sidebarTsx?: string;
}

export type StateKey = "plainOpen" | "floatOpen" | "plainClosed" | "floatClosed";

export interface PanelState {
  /** Panel width (border box) for the width the app sets in `--panel-w`. */
  width: (panelW: number) => number;
  marginLeft: number;
  borderLeft: number;
  borderRight: number;
}

export interface ShellModel {
  /** Outer padding of the floating row, px. */
  floatPad: number;
  /** Gap between neighbouring columns in the floating row, px. */
  floatGap: number;
  states: Record<StateKey, PanelState>;
  /** The panel's slide duration in ms (`--motion-duration-slow`), parsed with units. */
  slideMs: number;
  /** Inner column width for `--panel-w` = panelW in the given panel state. */
  innerWidth: (panelW: number, state: StateKey) => number;
}

export interface ContractReport {
  model: ShellModel;
  failures: string[];
}

const FILE = {
  sidebar: "packages/ui/src/components/Sidebar.module.css",
  tokens: "packages/ui/tokens.css",
  global: "apps/desktop/ui/src/global.css",
  theme: "packages/ui/src/theme.ts",
  tsx: "packages/ui/src/components/Sidebar.tsx"
};

const FLOAT = ':global(:root[data-float="on"])';
const OPEN = '.panel[data-open="true"]';
const OVERLAY = '.panel[data-mode="overlay"]';
const OVERLAY_OPEN = '.panel[data-mode="overlay"][data-open="true"]';

/** Reviewed selectors that may set geometry properties, in specificity order for the panel. */
const REVIEWED = new Set([
  ".foregroundContainer",
  `${FLOAT} .foregroundContainer`,
  ".mainPane",
  `${FLOAT} .mainPane`,
  ".sidebar",
  `${FLOAT} .sidebar`,
  ".panel",
  OPEN,
  `${FLOAT} .panel`,
  `${FLOAT} ${OPEN}`,
  OVERLAY,
  OVERLAY_OPEN,
  `${FLOAT} ${OVERLAY}`,
  `${FLOAT} ${OVERLAY_OPEN}`,
  ".panelInner"
]);
const PANEL_ORDER: Record<StateKey, string[]> = {
  plainClosed: [".panel"],
  plainOpen: [".panel", OPEN],
  floatClosed: [".panel", `${FLOAT} .panel`],
  floatOpen: [".panel", OPEN, `${FLOAT} .panel`, `${FLOAT} ${OPEN}`]
};

const CONTRACTED = /\.(foregroundContainer|mainPane|sidebar|panelInner|panel)\b/;
const GEOMETRY =
  /^(width|min-width|max-width|flex|flex-shrink|flex-grow|flex-basis|margin(-.*)?|padding(-.*)?|gap|row-gap|column-gap|border(-.*)?|box-sizing|overflow|overflow-x|position|top|bottom|left|right|inset(-.*)?|transform|translate|display|--panel-edge|--panel-w|--size-sidebar-width)$/;
const SUPPORTED = new Set([
  "width", "min-width", "flex", "flex-shrink", "margin", "margin-left", "padding", "gap",
  "border", "border-left", "border-right", "border-top", "border-bottom", "border-width",
  "border-left-width", "border-right-width", "border-style", "border-left-style",
  "border-right-style", "box-sizing", "overflow", "position", "top", "bottom", "right",
  "transform", "display", "--panel-edge"
]);

/** Comments become blanks of the same length (newlines kept), so line numbers stay true. */
const stripComments = (css: string) => css.replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, " "));
const norm = (s: string) => s.replace(/\s+/g, " ").trim();

export interface Rule {
  selector: string;
  /** Declarations in source order, duplicates kept. */
  list: Array<[string, string]>;
  decls: Map<string, string>;
}

export function parseRules(css: string): Rule[] {
  const rules: Rule[] = [];
  const text = stripComments(css);
  const re = /([^{}]+)\{([^{}]*)\}/g;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    const list: Array<[string, string]> = [];
    for (const part of m[2].split(";")) {
      const i = part.indexOf(":");
      if (i > 0) list.push([part.slice(0, i).trim(), norm(part.slice(i + 1))]);
    }
    const decls = new Map(list);
    for (const selector of m[1].split(",")) rules.push({ selector: norm(selector), list, decls });
  }
  return rules;
}

/** Custom properties declared on `:root` in tokens.css (the plain, unconditional block). */
function rootVars(tokensCss: string): Map<string, string> {
  const vars = new Map<string, string>();
  for (const r of parseRules(tokensCss)) {
    if (r.selector !== ":root") continue;
    for (const [k, v] of r.list) if (k.startsWith("--")) vars.set(k, v);
  }
  return vars;
}

/** Evaluate `12px`, `var(--x)`, `calc(a * n)`, `calc(a - b)`; undefined when it is not a plain length. */
export function evalLength(expr: string, vars: Map<string, string>, local = new Map<string, string>()): number | undefined {
  let text = norm(expr);
  for (let guard = 0; guard < 8 && text.includes("var("); guard++) {
    text = text.replace(/var\(\s*(--[\w-]+)\s*(?:,\s*((?:[^()]|\([^)]*\))*))?\)/g, (_m, name: string, fallback?: string) =>
      local.get(name) ?? vars.get(name) ?? fallback ?? "NaN"
    );
  }
  text = text.replace(/calc\(/g, "(").replace(/px/g, "");
  if (!/^[\d\s.+\-*/()]+$/.test(text)) return undefined;
  let pos = 0;
  const peek = (): string | undefined => {
    while (text[pos] === " ") pos++;
    return text[pos];
  };
  const number = (): number => {
    const c = peek();
    if (c === "(") {
      pos++;
      const v = sum();
      peek();
      pos++;
      return v;
    }
    if (c === "-") {
      pos++;
      return -number();
    }
    const m = /^\d*\.?\d+/.exec(text.slice(pos));
    if (!m) throw new Error("number");
    pos += m[0].length;
    return Number(m[0]);
  };
  const product = (): number => {
    let v = number();
    for (let c = peek(); c === "*" || c === "/"; c = peek()) {
      pos++;
      v = c === "*" ? v * number() : v / number();
    }
    return v;
  };
  const sum = (): number => {
    let v = product();
    for (let c = peek(); c === "+" || c === "-"; c = peek()) {
      pos++;
      v = c === "+" ? v + product() : v - product();
    }
    return v;
  };
  try {
    const v = sum();
    return Number.isFinite(v) ? v : undefined;
  } catch {
    return undefined;
  }
}

/** Split a value on spaces outside parentheses. */
function tokens(value: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let cur = "";
  for (const ch of value) {
    if (ch === "(") depth++;
    if (ch === ")") depth--;
    if (ch === " " && depth === 0) {
      if (cur) out.push(cur);
      cur = "";
    } else cur += ch;
  }
  if (cur) out.push(cur);
  return out;
}

type Side = "top" | "right" | "bottom" | "left";
const SIDES: Side[] = ["top", "right", "bottom", "left"];
/** top right bottom left from a 1-4 value shorthand. */
function box4<T>(v: T[]): [T, T, T, T] {
  if (v.length === 1) return [v[0], v[0], v[0], v[0]];
  if (v.length === 2) return [v[0], v[1], v[0], v[1]];
  if (v.length === 3) return [v[0], v[1], v[2], v[1]];
  return [v[0], v[1], v[2], v[3]];
}

class Evaluator {
  private readonly vars: Map<string, string>;
  private readonly fail: (msg: string) => void;
  private readonly where: string;

  constructor(vars: Map<string, string>, fail: (msg: string) => void, where: string) {
    this.vars = vars;
    this.fail = fail;
    this.where = where;
  }

  len(expr: string, local?: Map<string, string>): number | undefined {
    return evalLength(expr, this.vars, local);
  }

  /** Computed border widths (left/right) after applying the rules' declarations in order. */
  borders(rules: Rule[]): { left: number; right: number } {
    const w: Record<Side, number> = { top: 3, right: 3, bottom: 3, left: 3 };
    const style: Record<Side, boolean> = { top: false, right: false, bottom: false, left: false }; // true = visible
    const width = (t: string): number => {
      if (t === "thin") return 1;
      if (t === "medium") return 3;
      if (t === "thick") return 5;
      const v = this.len(t);
      if (v === undefined) this.fail(`${this.where}: cannot evaluate border width "${t}"`);
      return v ?? 0;
    };
    const isStyle = (t: string) => /^(none|hidden|solid|dashed|dotted|double|groove|ridge|inset|outset)$/.test(t);
    const shorthand = (sides: Side[], value: string) => {
      const t = tokens(value);
      const st = t.find(isStyle) ?? "none";
      const wd = t.find((x) => !isStyle(x) && /^(thin|medium|thick|calc\(|var\(|[\d.]+px|0$)/.test(x));
      for (const s of sides) {
        w[s] = wd ? width(wd) : 3;
        style[s] = st !== "none" && st !== "hidden";
      }
    };
    for (const rule of rules) {
      for (const [prop, value] of rule.list) {
        if (prop === "border") shorthand(SIDES, value);
        else if (/^border-(left|right|top|bottom)$/.test(prop)) shorthand([prop.slice(7) as Side], value);
        else if (prop === "border-width") box4(tokens(value).map(width)).forEach((v, i) => (w[SIDES[i]] = v));
        else if (/^border-(left|right)-width$/.test(prop)) w[prop.split("-")[1] as Side] = width(value);
        else if (prop === "border-style") box4(tokens(value)).forEach((v, i) => (style[SIDES[i]] = v !== "none" && v !== "hidden"));
        else if (/^border-(left|right)-style$/.test(prop)) style[prop.split("-")[1] as Side] = value !== "none" && value !== "hidden";
      }
    }
    return { left: style.left ? w.left : 0, right: style.right ? w.right : 0 };
  }

  /** Last value of a property over the rules (declaration order), or undefined. */
  last(rules: Rule[], prop: string): string | undefined {
    let v: string | undefined;
    for (const r of rules) for (const [p, val] of r.list) if (p === prop) v = val;
    return v;
  }

  marginLeft(rules: Rule[]): number {
    let m = 0;
    for (const r of rules) {
      for (const [p, val] of r.list) {
        if (p === "margin") m = this.len(box4(tokens(val))[3]) ?? 0;
        if (p === "margin-left") m = this.len(val) ?? 0;
      }
    }
    return m;
  }

  /** Horizontal padding total (0 expected on the panel). */
  paddingX(rules: Rule[]): number {
    let l = 0;
    let r_ = 0;
    for (const r of rules) {
      for (const [p, val] of r.list) {
        if (p === "padding") {
          const b = box4(tokens(val).map((x) => this.len(x) ?? 0));
          r_ = b[1];
          l = b[3];
        }
      }
    }
    return l + r_;
  }
}

/**
 * Read the contract.  `expect` carries the single-source numbers the TypeScript side exports
 * (`FLOAT_PAD`, `FLOAT_GAP`).
 */
export function readShellContract(
  src: CssSources,
  expect: { floatPad: number; floatGap: number }
): ContractReport {
  const failures: string[] = [];
  const fail = (file: string, rule: string, expected: string, got: string | undefined) =>
    failures.push(`${file} :: ${rule}: expected ${expected}, found ${got ?? "nothing"}`);
  const rules = parseRules(src.sidebarCss);
  const vars = rootVars(src.tokensCss);
  const ev = new Evaluator(vars, (m) => failures.push(`${FILE.sidebar} :: ${m}`), "border");

  // 1. Reject what the reader cannot follow: at-rule blocks that contain contracted selectors.
  //    Unrelated at-rules (a footer @media, keyframes) are fine.
  const clean = stripComments(src.sidebarCss);
  for (const m of clean.matchAll(/@(media|container|supports|layer|scope)\b[^{;]*\{/g)) {
    let depth = 1;
    let end = m.index + m[0].length;
    for (; end < clean.length && depth > 0; end++) {
      if (clean[end] === "{") depth++;
      if (clean[end] === "}") depth--;
    }
    const block = clean.slice(m.index + m[0].length, end);
    const hit = CONTRACTED.exec(block);
    if (hit) {
      const line = clean.slice(0, m.index).split("\n").length;
      fail(
        `${FILE.sidebar}:${line}`,
        `at-rule "${norm(m[0].slice(0, -1))}" contains a rule for the contracted shell selector ${hit[0]}`,
        "shell rules outside conditional at-rules (conditional overrides are not modelled): move the rule out of the at-rule, or extend shellContract.ts to model it",
        "an at-rule block"
      );
    }
  }
  // 2. Every geometry declaration on a contracted class sits in a reviewed selector, once, supported, no !important.
  const seen = new Map<string, number>();
  for (const r of rules) {
    if (!CONTRACTED.test(r.selector)) continue;
    for (const [prop, value] of r.list) {
      if (!GEOMETRY.test(prop) || /(color|radius)$/.test(prop)) continue;
      if (!REVIEWED.has(r.selector)) {
        fail(FILE.sidebar, `${r.selector} { ${prop} }`, "geometry declarations only in the reviewed shell rules (extra override rejected)", `${prop}: ${value}`);
        continue;
      }
      if (!SUPPORTED.has(prop)) fail(FILE.sidebar, `${r.selector} { ${prop} }`, "a property the contract models (see SUPPORTED)", `${prop}: ${value}`);
      if (/!important/.test(value)) fail(FILE.sidebar, `${r.selector} { ${prop} }`, "no !important", value);
      const key = `${r.selector}|${prop}`;
      seen.set(key, (seen.get(key) ?? 0) + 1);
    }
  }
  for (const [key, n] of seen) {
    if (n > 1) fail(FILE.sidebar, key.replace("|", " { ") + " }", "declared once (duplicate override rejected)", `${n} declarations`);
  }

  const pick = (selectors: string[]): Rule[] => {
    const out: Rule[] = [];
    for (const sel of selectors) out.push(...rules.filter((r) => r.selector === sel));
    return out;
  };

  // (a) float padding and gap, and nothing in the plain row.
  const row = pick([".foregroundContainer", `${FLOAT} .foregroundContainer`]);
  const plainRow = pick([".foregroundContainer"]);
  for (const prop of ["padding", "gap"]) {
    if (ev.last(plainRow, prop) !== undefined) fail(FILE.sidebar, `.foregroundContainer ${prop}`, "none outside the float rule", ev.last(plainRow, prop));
  }
  const padExpr = ev.last(row, "padding");
  const gapExpr = ev.last(row, "gap");
  const pad = padExpr === undefined ? undefined : ev.len(padExpr);
  const gap = gapExpr === undefined ? undefined : ev.len(gapExpr);
  if (pad !== expect.floatPad) fail(FILE.sidebar, `${FLOAT} .foregroundContainer padding`, `${expect.floatPad}px (FLOAT_PAD)`, padExpr);
  if (gap !== expect.floatGap) fail(FILE.sidebar, `${FLOAT} .foregroundContainer gap`, `${expect.floatGap}px (FLOAT_GAP)`, gapExpr);

  // (d) the chat column may shrink instead of overflowing.
  const main = pick([".mainPane", `${FLOAT} .mainPane`]);
  const minWidth = ev.last(main, "min-width");
  if (minWidth === undefined || ev.len(minWidth) !== 0) fail(FILE.sidebar, ".mainPane min-width", "var(--space-0) / 0", minWidth);
  if (ev.last(main, "flex") !== "1") fail(FILE.sidebar, ".mainPane flex", "1", ev.last(main, "flex"));

  // The rail: width from the token the app sets, never shrunk by flex.
  const sidebar = pick([".sidebar", `${FLOAT} .sidebar`]);
  if (ev.last(sidebar, "width") !== "var(--size-sidebar-width)") fail(FILE.sidebar, ".sidebar width", "var(--size-sidebar-width)", ev.last(sidebar, "width"));
  if (ev.last(sidebar, "flex-shrink") !== "0") fail(FILE.sidebar, ".sidebar flex-shrink", "0", ev.last(sidebar, "flex-shrink"));

  // (e) borders count inside the box.
  if (!/\*\s*,\s*\*::before\s*,\s*\*::after\s*\{[^}]*box-sizing:\s*border-box/.test(stripComments(src.globalCss))) {
    fail(FILE.global, "*, *::before, *::after box-sizing", "border-box", undefined);
  }
  // float flag of the theme engine: glass, tinted or a backdrop.
  if (!/root\.dataset\.float\s*=\s*backdropActive\s*\|\|\s*style\s*===\s*"glass"\s*\|\|\s*style\s*===\s*"tinted"/.test(src.themeTs)) {
    fail(FILE.theme, "data-float", 'backdropActive || style === "glass" || style === "tinted"', undefined);
  }
  // `--panel-w` is the panel's target width, set by the component from the `width` prop.
  if (src.sidebarTsx !== undefined && !/"--panel-w":\s*`\$\{width\}px`/.test(src.sidebarTsx)) {
    fail(FILE.tsx, "SidePanel style", '"--panel-w": `${width}px`', undefined);
  }

  // Panel states, evaluated through the cascade.
  const panelW0 = 320;
  const stateOf = (key: StateKey): PanelState => {
    const rs = pick(PANEL_ORDER[key]);
    const open = key.endsWith("Open");
    const isFloat = key.startsWith("float");
    for (const prop of ["flex-shrink", "overflow"]) {
      const want = prop === "flex-shrink" ? "0" : "hidden";
      if (ev.last(rs, prop) !== want) fail(FILE.sidebar, `.panel (${key}) ${prop}`, want, ev.last(rs, prop));
    }
    for (const prop of ["min-width", "max-width"]) {
      if (ev.last(rs, prop) !== undefined) fail(FILE.sidebar, `.panel (${key}) ${prop}`, "absent (the width contract is width only)", ev.last(rs, prop));
    }
    if (ev.paddingX(rs) !== 0) fail(FILE.sidebar, `.panel (${key}) padding`, "none (padding would shrink the content box)", ev.last(rs, "padding"));
    const widthExpr = ev.last(rs, "width");
    const width = (panelW: number): number => {
      if (widthExpr === undefined) return 0;
      return ev.len(widthExpr, new Map([["--panel-w", `${panelW}px`]])) ?? Number.NaN;
    };
    // Closed: 0.  Open: exactly the width the app sets.
    const want = open ? panelW0 : 0;
    if (width(panelW0) !== want) fail(FILE.sidebar, `.panel (${key}) width`, open ? "var(--panel-w) (the width the app sets)" : "0", widthExpr);
    const margin = ev.marginLeft(rs);
    const wantMargin = isFloat && !open ? -expect.floatGap : 0;
    if (margin !== wantMargin) fail(FILE.sidebar, `.panel (${key}) margin-left`, `${wantMargin}px`, String(margin));
    const b = ev.borders(rs);
    if (open && !isFloat && (b.right !== 0 || b.left < 0)) fail(FILE.sidebar, `.panel (${key}) border`, "border-left only", JSON.stringify(b));
    if (!open && (b.left !== 0 || b.right !== 0)) fail(FILE.sidebar, `.panel (${key}) border`, "none while closed", JSON.stringify(b));
    return { width, marginLeft: margin, borderLeft: b.left, borderRight: b.right };
  };
  const states: Record<StateKey, PanelState> = {
    plainClosed: stateOf("plainClosed"),
    plainOpen: stateOf("plainOpen"),
    floatClosed: stateOf("floatClosed"),
    floatOpen: stateOf("floatOpen")
  };

  // (b) the inner column: derived from the panel's content box, never its border box.
  const inner = pick([".panelInner"]);
  const innerExpr = ev.last(inner, "width");
  if (innerExpr === undefined || /^var\(--panel-w\)$/.test(innerExpr)) {
    fail(FILE.sidebar, ".panelInner width", "100% or calc(var(--panel-w) - var(--panel-edge)), not var(--panel-w) (it is clipped by the panel's own border)", innerExpr);
  }
  for (const prop of ["min-width", "max-width", "margin", "margin-left", "padding"]) {
    if (ev.last(inner, prop) !== undefined) fail(FILE.sidebar, `.panelInner ${prop}`, "absent (the inner column fills the content box)", ev.last(inner, prop));
  }
  const edgeOf = (key: StateKey): number | undefined => {
    const v = ev.last(pick(PANEL_ORDER[key]), "--panel-edge");
    return v === undefined ? undefined : ev.len(v);
  };
  const innerWidth: ShellModel["innerWidth"] = (panelW, key) => {
    if (innerExpr === undefined) return Number.NaN;
    const st = states[key];
    if (innerExpr === "100%") return st.width(panelW) - st.borderLeft - st.borderRight;
    const local = new Map<string, string>([["--panel-w", `${panelW}px`]]);
    const edge = edgeOf(key);
    if (edge !== undefined) local.set("--panel-edge", `${edge}px`);
    return ev.len(innerExpr, local) ?? Number.NaN;
  };

  // Transition: the rail's reserve is derived from this token; it must parse (ms or s) and drive the panel's slide.
  const slow = vars.get("--motion-duration-slow");
  const slideMs = parseDurationMs(slow);
  if (slideMs === undefined) fail(FILE.tokens, "--motion-duration-slow", "a <time> in ms or s (the reserve is derived from it)", slow);
  const transition = ev.last(pick([".panel"]), "transition") ?? "";
  for (const prop of ["width", "margin"]) {
    if (!new RegExp(`(^|,)\\s*${prop}\\s+var\\(--motion-duration-slow\\)`).test(transition)) {
      fail(FILE.sidebar, `.panel transition ${prop}`, `${prop} var(--motion-duration-slow) (the duration the rail reserve follows)`, transition);
    }
  }
  const normalMs = parseDurationMs(vars.get("--motion-duration-normal"));
  if (normalMs === undefined || normalMs < 180 || normalMs > 240) {
    fail(FILE.tokens, "--motion-duration-normal", "180ms..240ms for overlay panel slide", vars.get("--motion-duration-normal"));
  }
  const overlayRules = pick([OVERLAY]);
  if (ev.last(overlayRules, "position") !== "absolute") {
    fail(FILE.sidebar, `${OVERLAY} position`, "absolute (overlay never reflows chat or screen)", ev.last(overlayRules, "position"));
  }
  if (ev.last(overlayRules, "width") !== "var(--panel-w)") {
    fail(FILE.sidebar, `${OVERLAY} width`, "var(--panel-w)", ev.last(overlayRules, "width"));
  }
  const overlayTrans = ev.last(overlayRules, "transition") ?? "";
  for (const prop of ["transform", "opacity"]) {
    if (!new RegExp(`(^|,)\\s*${prop}\\s+var\\(--motion-duration-normal\\)`).test(overlayTrans)) {
      fail(FILE.sidebar, `${OVERLAY} transition ${prop}`, `${prop} var(--motion-duration-normal)`, overlayTrans);
    }
  }
  if (!/@media \(prefers-reduced-motion: reduce\)\s*\{[^}]*transition:\s*none\s*!important/.test(stripComments(src.globalCss))) {
    fail(FILE.global, "prefers-reduced-motion", "transition: none !important (the rail restores without delay)", undefined);
  }
  const resizing = rules.find((r) => r.selector === ":global(:root[data-window-resizing]) .panel");
  if (!resizing || resizing.decls.get("transition") !== "none") {
    fail(FILE.sidebar, ":global(:root[data-window-resizing]) .panel transition", "none (window resizes move the panel and the fitted rail together)", resizing?.decls.get("transition"));
  }

  return { failures, model: { floatPad: pad ?? expect.floatPad, floatGap: gap ?? expect.floatGap, states, slideMs: slideMs ?? Number.NaN, innerWidth } };
}
