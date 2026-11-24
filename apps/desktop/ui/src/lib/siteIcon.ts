/**
 * The small chip in front of a web step. It never calls a third-party favicon service (that would
 * tell a stranger which sites the bot visits): the chip is the site's first letter, or a globe
 * when there is no usable host. A real site icon, once the daemon serves one, plugs in here.
 */
export interface SiteIcon {
  kind: "letter" | "globe";
  /** Upper-case first letter of the site name (`letter` only). */
  letter: string;
}

export function siteIcon(host: string | null | undefined): SiteIcon {
  const first = (host ?? "").replace(/^www\./i, "").trim().charAt(0);
  return /\p{L}|\p{N}/u.test(first)
    ? { kind: "letter", letter: first.toLocaleUpperCase() }
    : { kind: "globe", letter: "" };
}
