import type { HomeAssistantSettings } from "../api/types";

/** What the owner typed into "Services allowed without asking", sorted into good and bad names. */
export interface ParsedServices {
  services: string[];
  invalid: string[];
}

const NAME = /^[a-z0-9_]{1,64}$/;

/** `domain.service`, lowercase. `homeassistant.*` is never listed: it acts on whatever it is pointed at. */
export function isServiceName(value: string): boolean {
  const [domain, service, ...rest] = value.split(".");
  return rest.length === 0 && domain !== "homeassistant" && NAME.test(domain ?? "") && NAME.test(service ?? "");
}

/** One service per line; commas and spaces separate too. Names are lowercased and listed once. */
export function parseServiceList(text: string): ParsedServices {
  const services: string[] = [];
  const invalid: string[] = [];
  for (const raw of text.split(/[\s,;]+/)) {
    const value = raw.trim().toLowerCase();
    if (value === "") continue;
    if (!isServiceName(value)) {
      if (!invalid.includes(value)) invalid.push(value);
    } else if (!services.includes(value)) {
      services.push(value);
    }
  }
  return { services, invalid };
}

export function formatServiceList(services: readonly string[]): string {
  return services.join("\n");
}

/** An address a person could open in a browser: http or https with a host, no login in it. */
export function looksLikeAddress(value: string): boolean {
  try {
    const url = new URL(value.trim());
    return (url.protocol === "http:" || url.protocol === "https:") && url.hostname !== "" && url.username === "" && url.password === "";
  } catch {
    return false;
  }
}

function target(value: string): string | null {
  try {
    const url = new URL(value.trim());
    return `${url.protocol}//${url.hostname.toLowerCase()}:${url.port || (url.protocol === "https:" ? "443" : "80")}${url.pathname.replace(/\/+$/, "")}`;
  } catch {
    return null;
  }
}

/**
 * Whether the form may be sent. The token is write-only: it can stay empty only while the
 * address is the saved one (the daemon then keeps the stored token and refuses otherwise).
 */
export function canSaveHomeAssistant(
  saved: Pick<HomeAssistantSettings, "configured" | "url" | "token_set"> | null,
  address: string,
  token: string,
  invalidServices: readonly string[]
): boolean {
  if (!looksLikeAddress(address) || invalidServices.length > 0) return false;
  if (token.trim() !== "") return true;
  return Boolean(saved?.configured && saved.token_set && saved.url && target(saved.url) === target(address));
}
