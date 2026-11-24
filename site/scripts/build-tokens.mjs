import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

/** The generated default (Obsidian) theme block of the desktop app's tokens.css. */
export function themeTokens() {
  const css = readFileSync(join(repo, "packages/ui/tokens.css"), "utf8");
  const generated = css.split("/* Generated default theme")[1];
  if (!generated) throw new Error("packages/ui/tokens.css has no generated theme block");
  return generated.slice(generated.indexOf(":root"));
}
