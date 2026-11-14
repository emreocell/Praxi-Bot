import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import { createElement, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { i18n as I18n } from "i18next";
import { createServer, type ViteDevServer } from "vite";
import type { AppSettings } from "../store/settingsStore.ts";
import type { OnboardingViewProps } from "../views/OnboardingView.tsx";
import type { CardDef, SettingsBridge } from "../views/settings/types.ts";

let server: ViteDevServer;
let i18n: I18n;
let defaults: AppSettings;
let generalCards: (bridge: SettingsBridge, t: I18n["t"]) => CardDef[];
let OnboardingView: ComponentType<OnboardingViewProps>;
let WidgetWindow: ComponentType;
const noop = () => {};

before(async () => {
  server = await createServer({
    root: fileURLToPath(new URL("../..", import.meta.url)),
    logLevel: "error",
    optimizeDeps: { noDiscovery: true, entries: [] },
    server: { middlewareMode: true, watch: null },
    ssr: { noExternal: ["ui"] },
    plugins: [{
      name: "onboarding-daemon-for-disclosure-test",
      enforce: "pre",
      resolveId(source, importer) {
        if (source === "../state/DaemonContext" && importer?.endsWith("/views/OnboardingView.tsx")) {
          return "\0onboarding-daemon-for-test";
        }
      },
      load(id) {
        if (id === "\0onboarding-daemon-for-test") {
          return 'export function useDaemon() { return { phase: "ready", error: null, conn: null, client: null, bots: [], events: { runs: {} } }; }';
        }
      }
    }]
  });
  ({ i18n } = await server.ssrLoadModule("/src/i18n/index.ts"));
  ({ DEFAULT_SETTINGS: defaults } = await server.ssrLoadModule("/src/store/settingsStore.ts"));
  ({ generalCards } = await server.ssrLoadModule("/src/views/settings/GeneralCards.tsx"));
  ({ OnboardingView } = await server.ssrLoadModule("/src/views/OnboardingView.tsx"));
  ({ WidgetWindow } = await server.ssrLoadModule("/src/overlay/WidgetWindow.tsx"));
});

after(async () => { await server?.close(); });

for (const [language, focus, widgetScope, stopButton] of [
  ["en", /focus/i, /Stop in app:/, /“Stop”/],
  ["tr", /odak/i, /Uygulamada durdur:/, /“Durdur”/]
] as const) {
  test(`${language} shortcut settings disclose the Windows-only global scope`, async () => {
    await i18n.changeLanguage(language);
    const cards = generalCards({
      settings: defaults, update: noop, onLanguage: noop, onWindowFrame: noop,
      recordingHotkey: false, onToggleRecordHotkey: noop, onStopHotkey: noop,
      onToggleAutostart: noop, autostartError: null, onOpenOnboarding: noop
    }, i18n.t.bind(i18n));
    const hotkey = cards.find((card) => card.id === "hotkey");
    assert.ok(hotkey);
    const html = renderToStaticMarkup(hotkey.node);
    assert.match(html, /Windows/);
    assert.match(html, focus);
    assert.doesNotMatch(hotkey.title, /global/i);
    assert.match(html, /Ctrl\+Alt\+Esc/);
  });

  test(`${language} onboarding explains shortcut focus and the actual Stop button`, async () => {
    await i18n.changeLanguage(language);
    const html = renderToStaticMarkup(createElement(OnboardingView, {
      settings: { ...defaults, onboardingStep: 3, stopHotkey: "Ctrl+Shift+Esc" },
      onUpdateSettings: noop, onLanguageChange: noop, onSkip: noop, onComplete: noop
    }));
    const hint = html.match(/<li>[^<]*Ctrl\+Shift\+Esc[^<]*<\/li>/)?.[0];
    assert.ok(hint, "the actual safety step renders the configured shortcut");
    assert.match(hint, /Windows/);
    assert.match(hint, focus);
    assert.match(hint, stopButton);
  });

  test(`${language} floating widget scopes its shortcut hint to the app`, async () => {
    await i18n.changeLanguage(language);
    const html = renderToStaticMarkup(createElement(WidgetWindow));
    assert.match(html, widgetScope);
    assert.match(html, /Ctrl\+Alt\+Esc/);
    assert.match(html, /title="[^"]*Windows[^"]*"/);
  });
}
