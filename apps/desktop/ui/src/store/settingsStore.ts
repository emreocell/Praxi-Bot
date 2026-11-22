import { load, type Store } from "@tauri-apps/plugin-store";
import { DEFAULT_STOP_HOTKEY, PRODUCT_CONFIG } from "../config/product";
import type { SupportedLanguage } from "../i18n";
import {
  DEFAULT_APPEARANCE,
  normalizeAppearance,
  type AppearanceSettings
} from "../lib/appearance";
import { parseBotOrder, parseHostBotOrders } from "../lib/botOrder";
import { parseMutedBots } from "../lib/botMute";
import { parseDigestTime } from "../lib/digestNotify";
import { DEFAULT_QUIET_HOURS, parseQuietHours, type QuietHours } from "../lib/quietHours";
import { defaultWindowFrame } from "../lib/windowChrome";

import { DEFAULT_VOICE_RATE, parseVoiceName, parseVoiceRate } from "../lib/voicePrefs";
import { type MessageTimes, parseMessageTimes } from "../lib/messageTime";
import {
  type ComputerTarget,
  parseComputerTarget,
  parseSendWith,
  type SendWith
} from "../lib/settingsPrefs";

/**
 * Stored under a new key: the old "windowFrame" held `true` for everyone (the
 * former default), which would keep Windows users on the native title bar.
 */
const WINDOW_FRAME_KEY = "windowFrameChoice";

export interface AppSettings extends AppearanceSettings {
  backgroundUrl: string;
  backgroundFileName: string;
  backgroundBlur: string;
  backgroundDim: string;
  language: SupportedLanguage;
  stopHotkey: string;
  onboardingCompleted: boolean;
  onboardingSkipped: boolean;
  onboardingStep: number;
  autostart: boolean;
  /** OS notification when an approval or question waits (window unfocused). */
  notifyNeedsYou: boolean;
  /** ... when a run longer than 30 s finishes (window unfocused). */
  notifyLongRun: boolean;
  /** ... when a run fails. */
  notifyFailed: boolean;
  /** Notifications carry only the bot name and a generic text. */
  notifyPrivate: boolean;
  /** Quiet hours: toasts are held and kept in the notification history. */
  quietHours: QuietHours;
  /** Daily digest notification time ("HH:MM"), or "" = off. */
  digestTime: string;
  /** The local day (YYYY-MM-DD) the digest was last handled, so it fires once a day. */
  digestLastDay: string;
  /** Bots whose long-run / failed notifications are muted (approvals still notify). */
  mutedBotIds: string[];
  /** Debug: also show the route chip for a plain `Agent` decision at K4. */
  showAgentRoute: boolean;
  /** Display name of the account shown in the sidebar menu. */
  accountName: string;
  /** Native window frame (title bar) on / off. */
  windowFrame: boolean;
  /** Key chord that sends a chat message. */
  sendWith: SendWith;
  /** How message times show in the chat. */
  messageTimes: MessageTimes;
  /** Where the bot's computer / browser runs. */
  computerTarget: ComputerTarget;
  /** Sidebar bot order (bot ids); the first bot is the main bot. Empty = daemon order. */
  botOrder: string[];
  /** Same, for each paired host (host id -> bot ids): every computer has its own main bot. */
  hostBotOrders: Record<string, string[]>;
  /** OS speech voice used to read replies aloud; "" = automatic (matches the UI language). */
  voiceName: string;
  /** Speaking speed of read-aloud replies (0.5 - 2). */
  voiceRate: number;
  /** Read each finished bot reply aloud automatically. */
  speakReplies: boolean;
  /** App version whose "What's new" card was last shown (or skipped); "" = never. */
  lastSeenVersion: string;
}

export const DEFAULT_SETTINGS: AppSettings = {
  ...DEFAULT_APPEARANCE,
  backgroundUrl: "",
  backgroundFileName: "",
  backgroundBlur: "0px",
  backgroundDim: "0",
  language: "en",
  stopHotkey: DEFAULT_STOP_HOTKEY,
  onboardingCompleted: false,
  onboardingSkipped: false,
  onboardingStep: 1,
  autostart: false,
  notifyNeedsYou: true,
  notifyLongRun: true,
  notifyFailed: true,
  notifyPrivate: false,
  quietHours: DEFAULT_QUIET_HOURS,
  mutedBotIds: [],
  digestTime: "",
  digestLastDay: "",
  showAgentRoute: false,
  accountName: "",
  windowFrame: defaultWindowFrame(typeof navigator === "undefined" ? "" : navigator.userAgent),
  sendWith: "enter",
  messageTimes: "minutes",
  computerTarget: "this-pc",
  botOrder: [],
  hostBotOrders: {},
  voiceName: "",
  voiceRate: DEFAULT_VOICE_RATE,
  speakReplies: false,
  lastSeenVersion: ""
};

export function shouldOpenOnboardingOnStart(
  settings: Pick<AppSettings, "onboardingCompleted" | "onboardingSkipped">
): boolean {
  return !settings.onboardingCompleted && !settings.onboardingSkipped;
}

const LOCAL_STORAGE_KEY = "desktop_ui_settings";

/** Appearance fields persisted (validated by `normalizeAppearance` on load). */
const APPEARANCE_KEYS = Object.keys(DEFAULT_APPEARANCE) as (keyof AppearanceSettings)[];

let storeInstancePromise: Promise<Store | null> | null = null;

function isTauriRuntime(): boolean {
  return (
    typeof window !== "undefined" &&
    ("__TAURI_INTERNALS__" in window || "__TAURI__" in window)
  );
}

async function getTauriStore(): Promise<Store | null> {
  if (!isTauriRuntime()) {
    return null;
  }
  if (!storeInstancePromise) {
    storeInstancePromise = load(PRODUCT_CONFIG.storeFileName, {
      defaults: { ...DEFAULT_SETTINGS },
      autoSave: true
    }).catch(() => null);
  }
  return storeInstancePromise;
}

/** Synchronous read of the localStorage mirror (written by every save); null if absent or malformed. */
export function loadSettingsSync(): AppSettings | null {
  try {
    const raw = typeof window !== "undefined" ? window.localStorage?.getItem(LOCAL_STORAGE_KEY) : null;
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<AppSettings>;
    return {
      ...DEFAULT_SETTINGS,
      ...parsed,
      ...normalizeAppearance(parsed as Record<string, unknown>),
      computerTarget: parseComputerTarget(parsed.computerTarget),
      messageTimes: parseMessageTimes(parsed.messageTimes),
      quietHours: parseQuietHours(parsed.quietHours),
      mutedBotIds: parseMutedBots(parsed.mutedBotIds),
      digestTime: parseDigestTime(parsed.digestTime),
      digestLastDay: typeof parsed.digestLastDay === "string" ? parsed.digestLastDay : "",
      botOrder: parseBotOrder(parsed.botOrder),
      hostBotOrders: parseHostBotOrders(parsed.hostBotOrders),
      voiceName: parseVoiceName(parsed.voiceName),
      voiceRate: parseVoiceRate(parsed.voiceRate),
      speakReplies: parsed.speakReplies === true,
      lastSeenVersion: typeof parsed.lastSeenVersion === "string" ? parsed.lastSeenVersion : ""
    };
  } catch {
    return null;
  }
}

export async function loadSettings(): Promise<AppSettings> {
  const store = await getTauriStore();
  if (store) {
    try {
      const appearance = normalizeAppearance(
        Object.fromEntries(
          await Promise.all(
            APPEARANCE_KEYS.map(async (k) => [k, await store.get<unknown>(k)] as const)
          )
        )
      );
      const backgroundUrl =
        (await store.get<string>("backgroundUrl")) ??
        DEFAULT_SETTINGS.backgroundUrl;
      const backgroundFileName =
        (await store.get<string>("backgroundFileName")) ??
        DEFAULT_SETTINGS.backgroundFileName;
      const backgroundBlur =
        (await store.get<string>("backgroundBlur")) ??
        DEFAULT_SETTINGS.backgroundBlur;
      const backgroundDim =
        (await store.get<string>("backgroundDim")) ??
        DEFAULT_SETTINGS.backgroundDim;
      const language =
        (await store.get<SupportedLanguage>("language")) ??
        DEFAULT_SETTINGS.language;
      const stopHotkey =
        (await store.get<string>("stopHotkey")) ?? DEFAULT_SETTINGS.stopHotkey;
      const onboardingCompleted =
        (await store.get<boolean>("onboardingCompleted")) ??
        DEFAULT_SETTINGS.onboardingCompleted;
      const onboardingSkipped =
        (await store.get<boolean>("onboardingSkipped")) ??
        DEFAULT_SETTINGS.onboardingSkipped;
      const onboardingStep =
        (await store.get<number>("onboardingStep")) ??
        DEFAULT_SETTINGS.onboardingStep;
      const autostart =
        (await store.get<boolean>("autostart")) ?? DEFAULT_SETTINGS.autostart;
      const notifyNeedsYou =
        (await store.get<boolean>("notifyNeedsYou")) ??
        DEFAULT_SETTINGS.notifyNeedsYou;
      const notifyLongRun =
        (await store.get<boolean>("notifyLongRun")) ??
        DEFAULT_SETTINGS.notifyLongRun;
      const notifyFailed =
        (await store.get<boolean>("notifyFailed")) ??
        DEFAULT_SETTINGS.notifyFailed;
      const quietHours = parseQuietHours(await store.get<unknown>("quietHours"));
      const mutedBotIds = parseMutedBots(await store.get<unknown>("mutedBotIds"));
      const digestTime = parseDigestTime(await store.get<unknown>("digestTime"));
      const digestLastDayRaw = await store.get<unknown>("digestLastDay");
      const digestLastDay = typeof digestLastDayRaw === "string" ? digestLastDayRaw : "";
      const notifyPrivate =
        (await store.get<boolean>("notifyPrivate")) ??
        DEFAULT_SETTINGS.notifyPrivate;
      const showAgentRoute =
        (await store.get<boolean>("showAgentRoute")) ??
        DEFAULT_SETTINGS.showAgentRoute;
      const accountName =
        (await store.get<string>("accountName")) ??
        DEFAULT_SETTINGS.accountName;
      const windowFrame =
        (await store.get<boolean>(WINDOW_FRAME_KEY)) ??
        DEFAULT_SETTINGS.windowFrame;
      const sendWith = parseSendWith(await store.get<string>("sendWith"));
      const messageTimes = parseMessageTimes(await store.get<string>("messageTimes"));
      const computerTarget = parseComputerTarget(
        await store.get<string>("computerTarget")
      );

      const botOrder = parseBotOrder(await store.get<unknown>("botOrder"));
      const hostBotOrders = parseHostBotOrders(await store.get<unknown>("hostBotOrders"));
      const voiceName = parseVoiceName(await store.get<string>("voiceName"));
      const voiceRate = parseVoiceRate(await store.get<number>("voiceRate"));
      const speakReplies = (await store.get<boolean>("speakReplies")) === true;
      const lastSeen = await store.get<string>("lastSeenVersion");
      const lastSeenVersion = typeof lastSeen === "string" ? lastSeen : "";

      return {
        ...appearance,
        backgroundUrl,
        backgroundFileName,
        backgroundBlur,
        backgroundDim,
        language,
        stopHotkey,
        onboardingCompleted,
        onboardingSkipped,
        onboardingStep,
        autostart,
        notifyNeedsYou,
        notifyLongRun,
        notifyFailed,
        notifyPrivate,
        quietHours,
        mutedBotIds,
        digestTime,
        digestLastDay,
        showAgentRoute,
        accountName,
        windowFrame,
        sendWith,
        messageTimes,
        computerTarget,
        botOrder,
        hostBotOrders,
        voiceName,
        voiceRate,
        speakReplies,
        lastSeenVersion
      };
    } catch {
      // Fall back to localStorage below if reading from plugin-store fails
    }
  }

  return loadSettingsSync() ?? { ...DEFAULT_SETTINGS };
}

export async function saveSettings(settings: AppSettings): Promise<void> {
  if (typeof window !== "undefined" && window.localStorage) {
    try {
      window.localStorage.setItem(LOCAL_STORAGE_KEY, JSON.stringify(settings));
    } catch {
      // Ignore storage quota errors
    }
  }

  const store = await getTauriStore();
  if (store) {
    try {
      for (const k of APPEARANCE_KEYS) await store.set(k, settings[k]);
      await store.set("backgroundUrl", settings.backgroundUrl);
      await store.set("backgroundFileName", settings.backgroundFileName);
      await store.set("backgroundBlur", settings.backgroundBlur);
      await store.set("backgroundDim", settings.backgroundDim);
      await store.set("language", settings.language);
      await store.set("stopHotkey", settings.stopHotkey);
      await store.set("onboardingCompleted", settings.onboardingCompleted);
      await store.set("onboardingSkipped", settings.onboardingSkipped);
      await store.set("onboardingStep", settings.onboardingStep);
      await store.set("autostart", settings.autostart);
      await store.set("notifyNeedsYou", settings.notifyNeedsYou);
      await store.set("notifyLongRun", settings.notifyLongRun);
      await store.set("notifyFailed", settings.notifyFailed);
      await store.set("notifyPrivate", settings.notifyPrivate);
      await store.set("quietHours", settings.quietHours);
      await store.set("mutedBotIds", settings.mutedBotIds);
      await store.set("digestTime", settings.digestTime);
      await store.set("digestLastDay", settings.digestLastDay);
      await store.set("showAgentRoute", settings.showAgentRoute);
      await store.set("accountName", settings.accountName);
      await store.set(WINDOW_FRAME_KEY, settings.windowFrame);
      await store.set("sendWith", settings.sendWith);
      await store.set("messageTimes", settings.messageTimes);
      await store.set("computerTarget", settings.computerTarget);
      await store.set("botOrder", settings.botOrder);
      await store.set("hostBotOrders", settings.hostBotOrders);
      await store.set("voiceName", settings.voiceName);
      await store.set("voiceRate", settings.voiceRate);
      await store.set("speakReplies", settings.speakReplies);
      await store.set("lastSeenVersion", settings.lastSeenVersion);
      await store.save();
    } catch {
      // Ignore store write errors in non-Tauri environments
    }
  }
}
