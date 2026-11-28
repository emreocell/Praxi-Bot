/**
 * Form rules of Settings > Notifications > "Approve payments from your phone" (ADR
 * docs/design/phone-money-approval-adr.md). The daemon checks the same rules again; these only
 * keep an obviously wrong form from being sent.
 */

/** 6 to 12 digits, as the daemon requires. */
export function pinIsValid(pin: string): boolean {
  return /^[0-9]{6,12}$/.test(pin);
}

/** `https://host[:port]` with no path, query or login (`http://` only on this computer). */
export function addressIsValid(raw: string): boolean {
  let url: URL;
  try {
    url = new URL(raw.trim());
  } catch {
    return false;
  }
  const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
  return (
    (url.protocol === "https:" || (url.protocol === "http:" && loopback)) &&
    url.username === "" &&
    url.password === "" &&
    url.search === "" &&
    url.hash === "" &&
    url.pathname === "/"
  );
}

/**
 * The i18n key of what is wrong with the form, or `null` when it can be sent. A PIN is required
 * the first time; once phone approval is on, an empty PIN keeps the current one.
 */
export function setupProblem(
  address: string,
  pin: string,
  repeat: string,
  enabled: boolean
): string | null {
  if (!addressIsValid(address)) return "phoneApproval.badAddress";
  if (pin === "" && repeat === "" && enabled) return null;
  if (!pinIsValid(pin)) return "phoneApproval.badPin";
  if (pin !== repeat) return "phoneApproval.pinMismatch";
  return null;
}

/** What a payment card's checkout page showed (`details.checkout`), as the card can print it. */
export interface CheckoutFacts {
  /** The host the bot's browser is on (the browser's own URL). */
  domain: string | null;
  /** The page's own claims: they can lie. */
  merchant: string | null;
  amount: string | null;
}

/** The checkout facts of a money card, or `null` when the card has none. */
export function checkoutFacts(details: unknown): CheckoutFacts | null {
  const d = details as { money_movement?: unknown; checkout?: unknown } | null | undefined;
  if (d?.money_movement !== true || !d.checkout || typeof d.checkout !== "object") return null;
  const c = d.checkout as Record<string, unknown>;
  const text = (v: unknown) => (typeof v === "string" && v.trim() !== "" ? v.slice(0, 120) : null);
  const facts = { domain: text(c.domain), merchant: text(c.merchant), amount: text(c.amount) };
  return facts.domain || facts.merchant || facts.amount ? facts : null;
}
