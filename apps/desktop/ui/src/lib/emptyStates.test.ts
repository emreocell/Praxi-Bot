import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const read = (f: string) => readFileSync(fileURLToPath(new URL(f, import.meta.url)), "utf8");
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const locale = (lng: string): Record<string, Record<string, any>> => JSON.parse(read(`../i18n/locales/${lng}.json`));

const EMPTY: Array<[string, string, string]> = [
  ["chatManage", "../components/ChatList.tsx", "chat-list-empty"],
  ["notifyHistory", "../views/NotificationHistoryView.tsx", "notify-history-empty"],
  ["needs", "../views/NeedsYouView.tsx", "needs-empty"],
  ["usage", "../views/UsageView.tsx", "EmptyState"],
  ["memory", "../views/settings/MemoryCard.tsx", "EmptyState"]
];

test("every list I own shows a titled empty state with one sentence of help, in en, tr and de", () => {
  const en = locale("en");
  for (const [ns, file, marker] of EMPTY) {
    assert.match(read(file), new RegExp(`<EmptyState[^>]*`), `${file} uses EmptyState`);
    assert.ok(read(file).includes(marker), `${file} ${marker}`);
    for (const lng of ["en", "tr", "de"]) {
      const l = locale(lng)[ns];
      assert.ok(l.emptyTitle && l.empty, `${lng}.${ns} title and sentence`);
      assert.ok(l.empty.split(/[.!?] /).length <= 3, `${lng}.${ns}.empty stays short`);
      if (lng !== "en") {
        assert.notEqual(l.emptyTitle, en[ns].emptyTitle);
        assert.notEqual(l.empty, en[ns].empty);
      }
    }
  }
});

test("the notification empty state offers its one action", () => {
  assert.match(read("../views/NotificationHistoryView.tsx"), /onAction=\{onOpenNeeds\}/);
});
