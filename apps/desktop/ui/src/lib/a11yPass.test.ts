import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const read = (rel: string) =>
  readFileSync(new URL(rel, import.meta.url), "utf8");

test("app shell renders a skip-to-main link before the sidebar and marks mainColumn as #main-content", () => {
  const app = read("../App.tsx");
  const appCss = read("../App.module.css");

  const skipIdx = app.indexOf('data-testid="skip-to-main"');
  const gripIdx = app.indexOf('label={t("shell.railGrip")}');
  assert.ok(skipIdx > 0, "skip-to-main link exists in App.tsx");
  assert.ok(skipIdx < gripIdx, "skip link comes before the sidebar resize grip");
  assert.match(app, /href="#main-content"/);
  assert.match(app, /id="main-content"\s+tabIndex=\{-1\}\s+className=\{styles\.mainColumn\}/);
  assert.match(appCss, /\.skipLink:focus-visible\s*\{[^}]*box-shadow:\s*var\(--shadow-focus\)/);
});

test("first-run wizard steps preserve heading hierarchy (h1 -> h2 without skipping to h3)", () => {
  const onboarding = read("../views/OnboardingView.tsx");
  const brain = read("../views/firstrun/BrainStep.tsx");
  const freedom = read("../views/firstrun/FreedomStep.tsx");

  assert.match(onboarding, /<h1\b/);
  // The first choices are a labelled group of buttons; the only section heading left is the local-servers one.
  assert.match(brain, /role="group" aria-label=\{t\("firstRun\.brain\.choices"\)\}/);
  assert.match(brain, /<h2 id="fr-local"/);
  assert.doesNotMatch(brain, /<h3 id="fr-(api|local)"/);
  assert.match(freedom, /<h2 id="fr-always"/);
  assert.doesNotMatch(freedom, /<h3 id="fr-always"/);
});

test("CommandPalette and Modal trap Tab focus, acquire appInert, and restore focus to a connected trigger", () => {
  const modal = read("../components/Modal.tsx");
  const palette = read("../components/CommandPalette.tsx");
  const accountMenu = read("../components/AccountMenu.tsx");
  const computerPicker = read("../components/ComputerPicker.tsx");

  assert.match(modal, /returnFocusTarget\(previous,\s*\(el\)\s*=>\s*el\.isConnected\)/);
  assert.match(palette, /appInert\.acquire\(seq\)/);
  assert.match(palette, /appInert\.release\(seq\)/);
  assert.match(palette, /trapTarget\(items,\s*current,\s*node,\s*e\.shiftKey\)/);
  assert.match(palette, /returnFocusTarget\(previous,\s*\(el\)\s*=>\s*el\.isConnected\)/);
  assert.match(
    accountMenu,
    /const run = \(id: AccountAction\) => \{\s*setOpen\(false\);\s*trigger\.current\?\.focus\(\);\s*onAction\(id\);\s*\}/
  );
  assert.match(
    computerPicker,
    /onClick=\{\(\) => \{\s*setOpen\(false\);\s*trigger\.current\?\.focus\(\);\s*onClick\(\);\s*\}\}/
  );
});

test("live regions announce streaming status politely, approvals assertively, and do not spam countdown ticks", () => {
  const runStatus = read("../components/RunStatusLine.tsx");
  const chatCards = read("../components/ChatCards.tsx");

  // The live region sits on the status text span (not wrapping action buttons or the 1s elapsed timer)
  assert.match(runStatus, /const urgent = approval \|\| stop\?\.phase === "failed" \|\| stop\?\.phase === "unconfirmed"/);
  assert.match(runStatus, /role=\{urgent \? "alert" : "status"\}/);
  assert.match(runStatus, /aria-live=\{urgent \? "assertive" : "polite"\}/);
  assert.match(runStatus, /aria-atomic="true"/);

  // Approval countdown is a timer with aria-live="off", never aria-live="polite"
  assert.match(chatCards, /className=\{styles\.countdown\}\s+role="timer"\s+aria-live="off"/);
  assert.doesNotMatch(chatCards, /className=\{styles\.countdown\}[^>]*aria-live="polite"/);
});

test("Settings nav, Segmented, ComputerTargetBlock, and CharacterPicker implement roving tabindex and arrow keys", () => {
  const settingsModal = read("../views/SettingsModal.tsx");
  const settingsUi = read("../components/SettingsUi.tsx");
  const computerCards = read("../views/settings/ComputerCards.tsx");
  const characterPicker = read("../components/CharacterPicker.tsx");

  assert.match(settingsModal, /onNavKeyDown/);
  assert.match(settingsModal, /e\.key !== "ArrowDown"/);
  assert.match(settingsUi, /role="radiogroup"[\s\S]*?onKeyDown=\{onKeyDown\}/);
  assert.match(settingsUi, /tabIndex=\{idx === activeIdx \? 0 : -1\}/);
  assert.match(computerCards, /role="radiogroup"[\s\S]*?onKeyDown=\{onKeyDown\}/);
  assert.match(computerCards, /tabIndex=\{target === activeTarget \? 0 : -1\}/);
  assert.match(characterPicker, /tabIndex=\{idx === activeIdx \? 0 : -1\}/);
});

test("focus rings use --shadow-focus / --color-focus across themes and interactive targets meet 24px (--space-6)", () => {
  const chatCss = read("../views/ChatView.module.css");
  const settingsCss = read("../views/SettingsModal.module.css");
  const botPickerCss = read("../components/BotIdentityPicker.module.css");
  const botPanelCss = read("../components/BotPanel.module.css");
  const gripCss = read("../components/ResizeGrip.module.css");
  const msgActionsCss = read("../components/MessageActions.module.css");
  const msgInfoCss = read("../components/MessageInfo.module.css");
  const runStatusCss = read("../components/RunStatusLine.module.css");

  // The composer shows focus with its border only (one quiet state, no ring on top of the border).
  assert.match(chatCss, /\.box:focus-within\s*\{[^}]*border-color:\s*var\(--color-focus\)/);
  assert.doesNotMatch(chatCss, /\.box:focus-within\s*\{[^}]*box-shadow/);
  assert.match(settingsCss, /\.searchWrap \.search:focus-visible\s*\{[^}]*box-shadow:\s*var\(--shadow-focus\)/);
  assert.match(botPickerCss, /\.nameInput:focus-visible\s*\{[^}]*box-shadow:\s*var\(--shadow-focus\)/);
  assert.match(botPanelCss, /\.chatSearch:focus-visible\s*\{[^}]*box-shadow:\s*var\(--shadow-focus\)/);
  assert.match(gripCss, /\.grip:focus-visible::after\s*\{[^}]*box-shadow:\s*var\(--shadow-focus\)/);
  assert.match(msgActionsCss, /\.copyBtn\s*\{[^}]*min-width:\s*var\(--space-6\);\s*min-height:\s*var\(--space-6\)/);
  assert.match(msgInfoCss, /\.info\s*\{[^}]*min-width:\s*var\(--space-6\);\s*min-height:\s*var\(--space-6\)/);
  assert.match(runStatusCss, /\.link\s*\{[^}]*min-height:\s*var\(--space-6\)/);
});

test("reduced-motion guards disable smooth scrolling and CSS animations when prefers-reduced-motion: reduce is set", () => {
  const chatView = read("../views/ChatView.tsx");
  const settingsModal = read("../views/SettingsModal.tsx");
  const popoverCss = read("../components/Popover.module.css");
  const gripCss = read("../components/ResizeGrip.module.css");
  const appearanceCss = read("../components/AppearancePicker.module.css");

  assert.doesNotMatch(chatView, /scrollIntoView\(\{[^}]*behavior:\s*"smooth"/);
  assert.match(chatView, /behavior:\s*instant\s*\?\s*"instant"\s*:\s*"smooth"/);
  assert.doesNotMatch(settingsModal, /scrollIntoView\(\{[^}]*behavior:\s*"smooth"/);
  assert.match(settingsModal, /behavior:\s*instant\s*\?\s*"instant"\s*:\s*"smooth"/);
  assert.match(popoverCss, /@media \(prefers-reduced-motion: reduce\)/);
  assert.match(gripCss, /@media \(prefers-reduced-motion: reduce\)/);
  assert.match(appearanceCss, /@media \(prefers-reduced-motion: reduce\)/);
});
