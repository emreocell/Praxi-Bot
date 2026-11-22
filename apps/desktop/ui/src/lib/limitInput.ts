/**
 * Strict readers for numbers typed into a limit field.
 *
 * `Number()` is too forgiving here: it reads "1.000" as one, and it also accepts "0x10" and "1e3".
 * A thousands separator is rejected instead of being taken for a decimal point, so a limit of one
 * thousand is never saved as one.
 */

/** Money: digits with at most two decimals; a comma or a dot is the decimal mark. */
export function parseUsdText(raw: string): number | null {
  const text = raw.trim();
  if (!/^\d+(?:[.,]\d{1,2})?$/.test(text)) {
    return null;
  }
  return Number(text.replace(",", "."));
}

/** A whole count (tokens): digits only, no separator, sign or exponent. */
export function parseCountText(raw: string): number | null {
  const text = raw.trim();
  return /^\d+$/.test(text) ? Number(text) : null;
}
