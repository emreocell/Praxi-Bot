/**
 * Layout probe: after the shell settled, measure the shell boxes and
 * append them as `<script id="layout-probe" type="application/json">` so a headless
 * `--dump-dom` run can read them without a debugging connection.  Enabled by `?probe=layout`.
 */

interface Box {
  left: number;
  right: number;
  top: number;
  bottom: number;
  width: number;
}

const box = (el: Element | null): Box | null => {
  if (!el) return null;
  const r = el.getBoundingClientRect();
  return { left: r.left, right: r.right, top: r.top, bottom: r.bottom, width: r.width };
};

function measure() {
  const part = (name: string) => document.querySelector(`[data-shell-part="${name}"]`);
  const rail = document.querySelector("[data-rail-tier]");
  const panel = part("panel");
  const inner = part("panel-inner");
  const cs = panel ? getComputedStyle(panel) : null;
  const borders = cs
    ? {
        left: parseFloat(cs.borderLeftWidth) || 0,
        right: parseFloat(cs.borderRightWidth) || 0,
        top: parseFloat(cs.borderTopWidth) || 0,
        bottom: parseFloat(cs.borderBottomWidth) || 0
      }
    : null;
  const panelBox = box(panel);
  const contentRight = panelBox && borders ? panelBox.right - borders.right : null;
  let worst: { tag: string; cls: string; right: number } | null = null;
  inner?.querySelectorAll("*").forEach((el) => {
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) return;
    if (!worst || r.right > worst.right) {
      worst = { tag: el.tagName.toLowerCase(), cls: String((el as HTMLElement).className).slice(0, 60), right: r.right };
    }
  });
  const root = document.documentElement;
  return {
    viewport: { width: window.innerWidth, height: window.innerHeight },
    float: root.dataset.float ?? null,
    corners: root.dataset.corners ?? null,
    backdrop: root.dataset.backdrop ?? null,
    style: root.dataset.style ?? null,
    theme: root.dataset.theme ?? null,
    container: box(part("main")?.parentElement ?? null),
    rail: box(rail),
    main: box(part("main")),
    panel: panelBox,
    panelOpenAttr: panel?.getAttribute("data-open") ?? null,
    panelDescendants: inner
      ? Array.from(inner.querySelectorAll("*")).filter((e) => e.getBoundingClientRect().width > 0).length
      : 0,
    panelBorders: borders,
    panelContentRight: contentRight,
    panelInner: box(inner),
    panelInnerScroll: inner ? { scrollWidth: inner.scrollWidth, clientWidth: inner.clientWidth } : null,
    panelRightMostDescendant: worst,
    document: { scrollWidth: root.scrollWidth, clientWidth: root.clientWidth }
  };
}

const POLL_MS = 50;
const DEADLINE_MS = 15000;
const pause = (ms: number) => new Promise((done) => setTimeout(done, ms));

/** The reasons the shell is not ready to be measured yet (empty = ready); the URL says what to expect. */
function notReady(): string[] {
  const q = new URLSearchParams(window.location.search);
  const wantOpen = q.get("panel") === "1";
  const root = document.documentElement;
  const part = (name: string) => document.querySelector(`[data-shell-part="${name}"]`);
  const missing: string[] = [];
  if (!part("main")) missing.push("main");
  if (!document.querySelector("[data-rail-tier]")) missing.push("rail");
  const panel = part("panel");
  if (!panel) missing.push("panel");
  const inner = part("panel-inner");
  if (!inner) missing.push("panel-inner");
  if (panel && panel.getAttribute("data-open") !== String(wantOpen)) missing.push(`panel data-open=${wantOpen}`);
  for (const [key, value] of [["theme", q.get("theme")], ["style", q.get("style")], ["corners", q.get("corners")]] as const) {
    if (value && root.dataset[key] !== value) missing.push(`${key}=${value}`);
  }
  if (wantOpen && inner && inner.querySelector("*") === null) missing.push("panel content");
  if (document.fonts && document.fonts.status !== "loaded") missing.push("fonts");
  return missing;
}

export async function runLayoutProbe(): Promise<void> {
  document.documentElement.dataset.probe = "started";
  const style = document.createElement("style");
  style.textContent = "*,*::before,*::after{transition:none!important;animation:none!important}";
  document.head.appendChild(style);
  // Wait for the requested state with a deadline (timers only: a busy backdrop canvas can starve
  // animation frames, and measuring forces layout anyway).  A missing shell is reported, not measured.
  let waited = 0;
  let missing = notReady();
  while (missing.length > 0 && waited < DEADLINE_MS) {
    await pause(POLL_MS);
    waited += POLL_MS;
    missing = notReady();
  }
  await pause(POLL_MS * 2);
  if (missing.length === 0) missing = notReady();
  const script = document.createElement("script");
  script.id = "layout-probe";
  script.type = "application/json";
  script.textContent = JSON.stringify(
    missing.length > 0 ? { error: `not ready after ${waited} ms: ${missing.join(", ")}` } : measure()
  );
  document.body.appendChild(script);
}

/**
 * Search probe (`?probe=search`): in a rail collapsed only by fitting, click Search and report where the
 * popover is, whether it is what the user would hit at its centre (not painted over), where focus is,
 * whether the stored rail width survived, and what Escape does.
 */
export async function runSearchProbe(): Promise<void> {
  await runLayoutProbe();
  const out: Record<string, unknown> = {};
  const button = document.querySelector<HTMLButtonElement>("[data-rail-tier] header button, [data-rail-tier] [class*=topActions] button");
  const storedRail = () => {
    try {
      return (JSON.parse(window.localStorage.getItem("praxi.shell.layout.v1") ?? "null") as { rail?: number } | null)?.rail ?? null;
    } catch {
      return null;
    }
  };
  out.railTier = document.querySelector("[data-rail-tier]")?.getAttribute("data-rail-tier") ?? null;
  out.railWidthBefore = document.querySelector("[data-rail-tier]")?.getBoundingClientRect().width ?? null;
  if (!button) {
    out.error = "search button not found";
  } else {
    button.click();
    await pause(400);
    const dialog = document.querySelector<HTMLElement>('body > [role="dialog"]');
    const input = dialog?.querySelector("input") ?? null;
    const r = dialog?.getBoundingClientRect();
    out.dialog = r ? { left: r.left, right: r.right, top: r.top, bottom: r.bottom } : null;
    out.dialogIsBodyChild = dialog?.parentElement === document.body;
    if (r) {
      const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
      out.hitInsideDialog = !!hit && !!dialog?.contains(hit);
      // The right part of the dialog is over the chat column: it must win there too.
      const hitRight = document.elementFromPoint(r.right - 8, r.top + r.height / 2);
      out.hitRightInsideDialog = !!hitRight && !!dialog?.contains(hitRight);
    }
    out.focusInInput = !!input && document.activeElement === input;
    out.railWidthAfterOpen = document.querySelector("[data-rail-tier]")?.getBoundingClientRect().width ?? null;
    out.storedRailAfterOpen = storedRail();
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    await pause(200);
    out.dialogGoneAfterEscape = document.querySelector('body > [role="dialog"]') === null;
    out.focusOnButtonAfterEscape = document.activeElement === button;
    out.storedRailAfterEscape = storedRail();
  }
  const script = document.createElement("script");
  script.id = "search-probe";
  script.type = "application/json";
  script.textContent = JSON.stringify(out);
  document.body.appendChild(script);
}

/**
 * Content probe (`?probe=content&lvState=paused&reacq=<label>`): at the narrowest fitted panel, measure the live-view
 * block's status row and recovery controls.  The mock has no stream drop, so the Reacquire button is cloned from the
 * Clear-pause button (same component, same classes) with the label passed in `reacq`.
 */
export async function runContentProbe(): Promise<void> {
  await runLayoutProbe();
  const out: Record<string, unknown> = {};
  const q = new URLSearchParams(window.location.search);
  let waited = 0;
  const actions = () => document.querySelector<HTMLElement>('[data-testid="bot-computer-paused-actions"]');
  while (!actions() && waited < DEADLINE_MS) {
    await pause(POLL_MS);
    waited += POLL_MS;
  }
  const row = actions();
  const clear = document.querySelector<HTMLElement>('[data-testid="bot-computer-clear-pause"]');
  const status = document.querySelector<HTMLElement>('[data-testid="bot-computer-status"]');
  const panel = document.querySelector<HTMLElement>('[data-shell-part="panel"]');
  if (!row || !clear || !status || !panel) {
    out.error = `live-view block not rendered (${[!row && "actions", !clear && "clear-pause", !status && "status", !panel && "panel"].filter(Boolean).join(", ")})`;
  } else {
    const reacq = q.get("reacq");
    if (reacq) {
      const clone = clear.cloneNode(true) as HTMLElement;
      clone.setAttribute("data-testid", "bot-computer-reacquire-probe");
      clone.textContent = reacq;
      row.insertBefore(clone, clear);
    }
    await pause(150);
    const pr = panel.getBoundingClientRect();
    const cs = getComputedStyle(panel);
    const contentRight = pr.right - (parseFloat(cs.borderRightWidth) || 0);
    const rect = (el: Element) => {
      const r = el.getBoundingClientRect();
      return { left: r.left, right: r.right, top: r.top, bottom: r.bottom };
    };
    out.contentRight = contentRight;
    out.panelWidth = pr.width;
    out.status = rect(status);
    out.statusText = status.textContent;
    out.buttons = Array.from(row.children).map((c) => ({ text: c.textContent, ...rect(c) }));
    out.row = { ...rect(row), scrollWidth: row.scrollWidth, clientWidth: row.clientWidth };
    const block = status.closest("section");
    out.block = block ? { ...rect(block), scrollWidth: block.scrollWidth, clientWidth: block.clientWidth } : null;
  }
  const script = document.createElement("script");
  script.id = "content-probe";
  script.type = "application/json";
  script.textContent = JSON.stringify(out);
  document.body.appendChild(script);
}
