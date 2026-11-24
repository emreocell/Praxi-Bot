import { ClipboardPaste, CornerDownLeft, ExternalLink, X } from "lucide-react";
import type { KeyboardEvent } from "react";
import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ApiClient } from "../api/client";
import { subscribeEvents } from "../api/events";
import type { Bot } from "../api/types";
import { i18n } from "../i18n";
import { humanLine } from "../lib/humanError";
import {
  buildQuickMessage,
  canSend,
  clipClipboard,
  EMPTY_STREAM,
  phaseOf,
  pickBot,
  QUESTION_LIMIT,
  reduceStream,
  type QuickStream
} from "../lib/quickAsk";
import { useApplyAppearance } from "../state/useApplyAppearance";
import { DEFAULT_SETTINGS, loadSettings, loadSettingsSync, type AppSettings } from "../store/settingsStore";
import { onQuickAskShown, quickAskConnect, quickAskHide, quickAskOpenInApp } from "./bridge";
import styles from "./QuickAskWindow.module.css";

const LAST_BOT_KEY = "praxi.quickask.bot";

function savedBot(): string | null {
  try {
    return window.localStorage.getItem(LAST_BOT_KEY);
  } catch {
    return null;
  }
}

/**
 * The Quick ask window: one box, a bot picker and the streaming answer. Each question starts a
 * new chat with the chosen bot. It never reads the clipboard unless the user presses
 * "Include clipboard", and it cannot approve anything: a run that needs an approval says so and
 * points to the app.
 */
export function QuickAskWindow() {
  const { t } = useTranslation();
  const [settings, setSettings] = useState<AppSettings>(() => loadSettingsSync() ?? DEFAULT_SETTINGS);
  useApplyAppearance(settings, undefined, true);

  const [client, setClient] = useState<ApiClient | null>(null);
  const [connectError, setConnectError] = useState<string | null>(null);
  const [bots, setBots] = useState<Bot[]>([]);
  const [botId, setBotId] = useState<string | null>(null);
  const [question, setQuestion] = useState("");
  const [clipboard, setClipboard] = useState<string | null>(null);
  const [clipboardDenied, setClipboardDenied] = useState(false);
  const [chatId, setChatId] = useState<string | null>(null);
  const [runId, setRunId] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [stream, dispatch] = useReducer(
    (s: QuickStream, e: Parameters<typeof reduceStream>[1]) => reduceStream(s, e),
    EMPTY_STREAM
  );
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    void loadSettings().then((loaded) => {
      setSettings(loaded);
      void i18n.changeLanguage(loaded.language);
    });
  }, []);

  // Connect once; a failure is shown in words and retried the next time the window opens.
  const connect = useCallback(async () => {
    try {
      const conn = await quickAskConnect();
      const api = new ApiClient(conn);
      setClient((old) => {
        old?.dispose();
        return api;
      });
      setConnectError(null);
      const list = await api.listBots();
      setBots(list);
      setBotId((current) => pickBot(list, current ?? savedBot()));
    } catch (e) {
      setConnectError(humanLine(e));
    }
  }, []);
  useEffect(() => {
    void connect();
  }, [connect]);

  // Live answer for the run we started.
  useEffect(() => {
    if (!client) return undefined;
    return subscribeEvents(client, {
      lastEventId: null,
      onEvent: (env) => dispatch(env.event),
      onConnection: () => {}
    });
  }, [client]);

  // Every time the window is shown: empty box, clipboard OFF again, cursor in the box.
  useEffect(
    () =>
      onQuickAskShown(() => {
        setQuestion("");
        setClipboard(null);
        setClipboardDenied(false);
        setSendError(null);
        if (!client) void connect();
        inputRef.current?.focus();
      }),
    [client, connect]
  );
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const phase = phaseOf(stream, runId, sending, sendError !== null);
  const run = runId ? stream.runs[runId] : undefined;

  const send = async () => {
    if (!client || !botId || sending || !canSend(question)) return;
    setSending(true);
    setSendError(null);
    try {
      const chat = await client.createChat(botId, question.trim().slice(0, 60));
      setChatId(chat.id);
      setRunId(null);
      const res = await client.sendMessage(chat.id, buildQuickMessage(question, clipboard));
      setRunId(res.run_id ?? null);
      try {
        window.localStorage.setItem(LAST_BOT_KEY, botId);
      } catch {
        // Not remembering the bot is harmless.
      }
      setQuestion("");
      setClipboard(null);
    } catch (e) {
      setSendError(humanLine(e));
    } finally {
      setSending(false);
    }
  };

  // The one place the clipboard is read: a click on "Include clipboard".
  const toggleClipboard = async () => {
    if (clipboard !== null) {
      setClipboard(null);
      return;
    }
    try {
      const text = clipClipboard(await navigator.clipboard.readText());
      setClipboardDenied(text === null);
      setClipboard(text);
    } catch {
      setClipboardDenied(true);
    }
  };

  const onKeyDown = (e: KeyboardEvent<HTMLElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      void quickAskHide();
    } else if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void send();
    }
  };

  const answerText = run?.text ?? "";
  const status =
    phase === "sending" || (phase === "streaming" && !answerText)
      ? t("quickAsk.thinking")
      : phase === "needs_you"
        ? t("quickAsk.needsYou")
        : phase === "error"
          ? (sendError ?? run?.error ?? t("quickAsk.failed"))
          : null;

  return (
    <div className={styles.viewport} onKeyDown={onKeyDown} role="dialog" aria-label={t("quickAsk.title")}>
      <div className={styles.card}>
        <div className={styles.header} data-tauri-drag-region>
          <span className={styles.title}>{t("quickAsk.title")}</span>
          <select
            className={styles.picker}
            aria-label={t("quickAsk.bot")}
            value={botId ?? ""}
            disabled={bots.length === 0}
            onChange={(e) => setBotId(e.target.value)}
          >
            {bots.map((b) => (
              <option key={b.id} value={b.id}>
                {b.name}
              </option>
            ))}
          </select>
          <button type="button" className={styles.iconButton} aria-label={t("quickAsk.close")} title={t("quickAsk.close")} onClick={() => void quickAskHide()}>
            <X size={16} aria-hidden />
          </button>
        </div>

        <div className={styles.inputRow}>
          <input
            ref={inputRef}
            className={styles.input}
            value={question}
            maxLength={QUESTION_LIMIT}
            placeholder={t("quickAsk.placeholder")}
            aria-label={t("quickAsk.placeholder")}
            onChange={(e) => setQuestion(e.target.value)}
          />
          <button
            type="button"
            className={styles.send}
            disabled={!client || !botId || sending || !canSend(question)}
            aria-label={t("quickAsk.send")}
            title={t("quickAsk.send")}
            onClick={() => void send()}
          >
            <CornerDownLeft size={16} aria-hidden />
          </button>
        </div>

        <div className={styles.options}>
          <button
            type="button"
            className={styles.clip}
            aria-pressed={clipboard !== null}
            onClick={() => void toggleClipboard()}
          >
            <ClipboardPaste size={14} aria-hidden />
            {t("quickAsk.includeClipboard")}
          </button>
          {clipboard !== null ? (
            <span className={styles.clipPreview} title={clipboard}>
              {clipboard.slice(0, 80)}
            </span>
          ) : clipboardDenied ? (
            <span className={styles.muted}>{t("quickAsk.clipboardEmpty")}</span>
          ) : (
            <span className={styles.muted}>{t("quickAsk.clipboardOff")}</span>
          )}
        </div>

        <div className={styles.answer} aria-live="polite">
          {connectError ? (
            <p className={styles.error}>{t("quickAsk.noService", { reason: connectError })}</p>
          ) : bots.length === 0 && client ? (
            <p className={styles.muted}>{t("quickAsk.noBots")}</p>
          ) : null}
          {answerText ? <p className={styles.answerText}>{answerText}</p> : null}
          {status ? <p className={phase === "error" ? styles.error : styles.muted}>{status}</p> : null}
        </div>

        <div className={styles.footer}>
          <span className={styles.muted}>{t("quickAsk.hints")}</span>
          <button
            type="button"
            className={styles.open}
            disabled={!chatId}
            onClick={() => void quickAskOpenInApp(chatId)}
          >
            <ExternalLink size={14} aria-hidden />
            {t("quickAsk.openInApp")}
          </button>
        </div>
      </div>
    </div>
  );
}
