import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? sources(join(dir, e.name)) : /\.tsx$/.test(e.name) ? [join(dir, e.name)] : []
  );
}

test("components and views never show a raw exception message to the user", () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const offenders: string[] = [];
  for (const dir of ["components", "views"]) {
    for (const file of sources(join(root, dir))) {
      if (/instanceof Error \? \w+\.message : String\(/.test(readFileSync(file, "utf8"))) {
        offenders.push(file.slice(root.length).replaceAll("\\", "/"));
      }
    }
  }
  // Route errors through humanLine()/humanError() so the user gets a title and a next step.
  // The coding CLIs block shows the raw text only as collapsible "details" under a human message.
  // BrainStep only classifies the text; CodingClisBlock keeps it as copyable details.
  assert.deepEqual(offenders, ["views/firstrun/BrainStep.tsx", "views/settings/CodingClisBlock.tsx"]);
});
