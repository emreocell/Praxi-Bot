/**
 * Validation of one browser layout probe.  Pure, so the failure paths are unit-tested:
 * a probe that did not measure the requested UI (missing shell, wrong open state, wrong appearance,
 * empty panel, non-finite or zero-size boxes, or a probe error) is a FAILURE, never a pass.
 */

export interface ProbeBox {
  left: number;
  right: number;
  top: number;
  bottom: number;
  width: number;
}

export interface ProbeData {
  error?: string;
  viewport: { width: number; height: number };
  float: string | null;
  style: string | null;
  theme: string | null;
  corners: string | null;
  backdrop: string | null;
  container: ProbeBox | null;
  rail: ProbeBox | null;
  main: ProbeBox | null;
  panel: ProbeBox | null;
  panelOpenAttr: string | null;
  panelBorders: { left: number; right: number; top: number; bottom: number } | null;
  panelContentRight: number | null;
  panelInner: ProbeBox | null;
  panelInnerScroll: { scrollWidth: number; clientWidth: number } | null;
  panelDescendants: number;
  panelRightMostDescendant: { tag: string; cls: string; right: number } | null;
  document: { scrollWidth: number; clientWidth: number };
}

export interface ProbeExpect {
  open: boolean;
  theme?: string;
  style?: string;
  corners?: string;
  /** `on` / `off`: a backdrop scene is expected. */
  backdrop?: string;
}

const EPS = 0.5;
const sane = (b: ProbeBox | null | undefined): b is ProbeBox =>
  !!b && [b.left, b.right, b.top, b.bottom, b.width].every(Number.isFinite) && b.width > 0 && b.bottom > b.top;

export function validateProbe(p: ProbeData | null | undefined, expect: ProbeExpect): string[] {
  if (!p || typeof p !== "object") return ["no probe data"];
  if (p.error) return [`probe did not measure the requested UI: ${p.error}`];
  const out: string[] = [];
  const vw = p.viewport?.width;
  if (!Number.isFinite(vw) || vw <= 0) return ["viewport width missing"];
  // Readiness: everything required is present, sized and in the requested state.
  const required: Array<[string, ProbeBox | null | undefined]> = [
    ["container", p.container],
    ["rail", p.rail],
    ["main", p.main]
  ];
  if (expect.open) required.push(["panel", p.panel], ["panelInner", p.panelInner]);
  for (const [name, b] of required) if (!sane(b)) out.push(`required ${name} missing, non-finite or zero-size`);
  if (expect.open !== (p.panelOpenAttr === "true")) out.push(`panel open state ${p.panelOpenAttr} != expected ${expect.open}`);
  if (expect.theme && p.theme !== expect.theme) out.push(`theme ${p.theme} != expected ${expect.theme}`);
  if (expect.style && p.style !== expect.style) out.push(`style ${p.style} != expected ${expect.style}`);
  if (expect.corners && p.corners !== expect.corners) out.push(`corners ${p.corners} != expected ${expect.corners}`);
  if (expect.backdrop && p.backdrop !== expect.backdrop) out.push(`backdrop ${p.backdrop} != expected ${expect.backdrop}`);
  if (expect.open && !(p.panelDescendants > 0)) out.push("panel has no rendered content");
  const doc = p.document;
  if (!(doc && Number.isFinite(doc.scrollWidth) && Number.isFinite(doc.clientWidth))) out.push("document widths missing");
  if (out.length) return out;

  // Geometry (all required boxes are present and sane from here on).
  const rail = p.rail as ProbeBox;
  const main = p.main as ProbeBox;
  const panel = p.panel;
  const inner = p.panelInner;
  const open = expect.open;
  const boxes: Array<[string, ProbeBox | null]> = [
    ["rail", rail],
    ["main", main],
    ["panel", open ? panel : null],
    ["panelInner", open ? inner : null]
  ];
  for (const [k, b] of boxes) {
    if (b && (b.left < -EPS || b.right > vw + EPS)) out.push(`${k} outside viewport (${b.left}..${b.right} of ${vw})`);
  }
  if (doc.scrollWidth > doc.clientWidth + EPS) out.push(`document scrolls horizontally (${doc.scrollWidth} > ${doc.clientWidth})`);
  if (rail.right > main.left + EPS) out.push("rail overlaps main");
  if (open && panel && main.right > panel.left + EPS) out.push("main overlaps panel");
  if (vw >= 900 && main.width < 320 - EPS) out.push(`main ${main.width.toFixed(1)} < 320`);
  if (main.width < 280 - EPS) out.push(`main ${main.width.toFixed(1)} < 280`);
  if (open && panel && inner) {
    const pad = p.float === "on" ? 12 : 0;
    if (Math.abs(panel.right - (vw - pad)) > EPS) out.push(`panel.right ${panel.right} != ${vw - pad}`);
    const cr = p.panelContentRight;
    if (cr === null || !Number.isFinite(cr)) out.push("panel content right missing");
    else {
      if (inner.right > cr + EPS) out.push(`panelInner.right ${inner.right} > panel content right ${cr} (clipped ${(inner.right - cr).toFixed(1)}px)`);
      if (Math.abs(inner.right - cr) > EPS) out.push(`panelInner.right ${inner.right} != panel content right ${cr}`);
      const d = p.panelRightMostDescendant;
      if (d && d.right > cr + EPS) out.push(`panel descendant <${d.tag}> right ${d.right} > ${cr}`);
    }
    const sc = p.panelInnerScroll;
    if (sc && sc.scrollWidth > sc.clientWidth + EPS) out.push(`panelInner scrolls horizontally (${sc.scrollWidth} > ${sc.clientWidth})`);
  }
  return out;
}
