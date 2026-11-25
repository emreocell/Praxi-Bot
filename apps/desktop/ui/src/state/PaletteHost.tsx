import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Schedule } from "../api/types";
import {
  CommandPalette,
  type PaletteCommand
} from "../components/CommandPalette";
import { useToast } from "../components/Toast";
import { botView } from "../lib/botProfile";
import { chatTitleLabel } from "../lib/chatTitle";
import { recentChats, settingsPageIds } from "../lib/paletteEntries";
import type { SettingsPageId } from "../lib/settingsPages";
import { useDaemon } from "./DaemonContext";
import { humanLine } from "../lib/humanError";

export type PaletteScreen =
  | "home"
  | "myBots"
  | "needs"
  | "schedules"
  | "usage"
  | "settings";

const SCREENS: readonly PaletteScreen[] = [
  "home",
  "myBots",
  "needs",
  "schedules",
  "usage",
  "settings"
];

/** Owns the Ctrl/Cmd+K shortcut and builds the palette's commands. */
export function PaletteHost({
  currentBotId,
  onScreen,
  onBot,
  onChat,
  onSettingsPage,
  onHelp
}: {
  currentBotId: string | undefined;
  onScreen: (screen: PaletteScreen) => void;
  onBot: (botId: string) => void;
  onChat: (chatId: string, botId: string) => void;
  onSettingsPage: (page: SettingsPageId) => void;
  onHelp: () => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, bots, chats, refreshChats } = useDaemon();
  const [open, setOpen] = useState(false);
  const [schedules, setSchedules] = useState<Schedule[]>([]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (
        (e.ctrlKey || e.metaKey) &&
        !e.altKey &&
        !e.shiftKey &&
        e.key.toLowerCase() === "k"
      ) {
        e.preventDefault();
        setOpen((o) => !o);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    if (!open || !client) {
      return;
    }
    let cancelled = false;
    client
      .listSchedules()
      .then((s) => {
        if (!cancelled) {
          setSchedules(s);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [open, client]);

  const newChat = useCallback(
    async (botId: string) => {
      if (!client) {
        return;
      }
      try {
        const chat = await client.createChat(botId);
        await refreshChats().catch(() => {});
        onChat(chat.id, botId);
      } catch (e) {
        toast.show(humanLine(e), "danger");
      }
    },
    [client, refreshChats, onChat, toast]
  );

  const commands = useMemo<PaletteCommand[]>(() => {
    const out: PaletteCommand[] = [];
    for (const s of SCREENS) {
      out.push({
        id: `go-${s}`,
        title: t(`palette.go.${s}`),
        group: t("palette.group.navigate"),
        run: () => onScreen(s)
      });
    }
    out.push({
      id: "help-center",
      title: t("palette.help"),
      group: t("palette.group.navigate"),
      keywords: t("palette.helpKeywords").split(" "),
      run: onHelp
    });
    out.push({
      id: "help-shortcuts",
      title: t("palette.shortcuts"),
      group: t("palette.group.navigate"),
      keywords: [t("help.title")],
      run: onHelp
    });
    const botId = currentBotId ?? bots[0]?.id;
    if (botId) {
      out.push({
        id: "new-chat",
        title: t("palette.newChat"),
        group: t("palette.group.chat"),
        keywords: ["new chat", "yeni sohbet"],
        run: () => void newChat(botId)
      });
    }
    for (const b of bots) {
      const name = botView(b).name;
      out.push({
        id: `bot-${b.id}`,
        title: t("palette.switchBot", { name }),
        group: t("palette.group.bot"),
        keywords: [name],
        run: () => onBot(b.id)
      });
    }
    for (const c of recentChats(chats)) {
      const bot = bots.find((b) => b.id === c.bot_id);
      const botName = bot ? botView(bot).name : "";
      const title = chatTitleLabel(c.title, t("botPanel.untitled"));
      out.push({
        id: `chat-${c.id}`,
        title,
        group: t("palette.group.recentChat"),
        keywords: botName ? [botName] : [],
        run: () => onChat(c.id, c.bot_id)
      });
    }
    for (const page of settingsPageIds()) {
      out.push({
        id: `settings-${page}`,
        title: t("palette.settingsPage", { name: t(`settingsModal.pages.${page}.name`) }),
        group: t("palette.group.settings"),
        keywords: [t(`settingsModal.pages.${page}.description`)],
        run: () => onSettingsPage(page)
      });
    }
    for (const s of schedules) {
      out.push({
        id: `sched-${s.id}`,
        title: t("palette.runSchedule", { name: s.name }),
        group: t("palette.group.schedule"),
        keywords: [s.name],
        run: () => {
          void client
            ?.runScheduleNow(s.id)
            .then(() => toast.show(t("palette.scheduleStarted"), "success"))
            .catch((e: unknown) =>
              toast.show(humanLine(e), "danger")
            );
        }
      });
    }
    return out;
  }, [t, bots, chats, schedules, currentBotId, onScreen, onBot, onChat, onSettingsPage, onHelp, newChat, client, toast]);

  return open ? (
    <CommandPalette commands={commands} onClose={() => setOpen(false)} />
  ) : null;
}
