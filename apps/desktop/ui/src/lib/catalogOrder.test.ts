import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import type { ComposioToolkit } from "../api/types";
import { CATALOG_PAGE_SIZE, POPULAR_APPS, catalogSections } from "./catalogOrder.ts";

const kit = (slug: string, group: string): ComposioToolkit => ({
  slug, name: slug, description: "", categories: [], tools_count: null, group, logo: null,
  native_first: false, state: "not_connected", enabled: false
});

test("the popular row leads in the owner's order, then categories in chip order", () => {
  const items = [
    kit("github", "dev"), kit("instagram", "social"), kit("gmail", "communication"), kit("asana", "productivity"),
    kit("slack", "communication"), kit("googlecalendar", "productivity"), kit("stripe", "finance"),
    kit("discord", "communication"), kit("mystery", "nonsense"), kit("outlook", "communication")
  ];
  const sections = catalogSections(items);
  assert.deepEqual(sections.map((s) => s.id), ["popular", "communication", "social", "productivity", "finance", "other"]);
  assert.deepEqual(sections[0].items.map((i) => i.slug), ["gmail", "googlecalendar", "outlook", "slack", "github"]);
  // Inside a category the daemon's order is kept; an unknown group lands in "other".
  assert.deepEqual(sections[1].items.map((i) => i.slug), ["discord"]);
  assert.deepEqual(sections.at(-1)?.items.map((i) => i.slug), ["mystery"]);
  // Every app shows exactly once.
  assert.equal(sections.flatMap((s) => s.items).length, items.length);
});

test("an empty page or one without popular apps has no empty sections", () => {
  assert.deepEqual(catalogSections([]), []);
  assert.deepEqual(catalogSections([kit("zoom", "communication")]).map((s) => s.id), ["communication"]);
});

test("the daemon's pinned list starts with the same popular apps in the same order", () => {
  const curated = JSON.parse(
    readFileSync(new URL("../../../../../crates/composio/src/curated.json", import.meta.url), "utf8")
  ) as { popular: { slug: string }[] };
  assert.deepEqual(curated.popular.slice(0, POPULAR_APPS.length).map((p) => p.slug), [...POPULAR_APPS]);
  assert.ok(CATALOG_PAGE_SIZE > POPULAR_APPS.length && CATALOG_PAGE_SIZE <= 100, "the daemon clamps a page to 100");
});
