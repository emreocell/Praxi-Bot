import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Schedule } from "../api/types";
import {
  CommandPalette,
  type PaletteCommand
} from "../components/CommandPalette";
import { useToast } from "../components/Toast";
import { botView } from "../lib/botProfile";
import { useDaemon } from "./DaemonContext";

export type PaletteScreen =
  | "home"
  | "needs"
  | "schedules"
  | "usage"
  | "settings";

const SCREENS: readonly PaletteScreen[] = [
  "home",
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
  onChat
}: {
  currentBotId: string | undefined;
  onScreen: (screen: PaletteScreen) => void;
  onBot: (botId: string) => void;
  onChat: (chatId: string, botId: string) => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, bots, refreshChats } = useDaemon();
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
        toast.show(e instanceof Error ? e.message : String(e), "danger");
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
              toast.show(e instanceof Error ? e.message : String(e), "danger")
            );
        }
      });
    }
    return out;
  }, [t, bots, schedules, currentBotId, onScreen, onBot, newChat, client, toast]);

  return open ? (
    <CommandPalette commands={commands} onClose={() => setOpen(false)} />
  ) : null;
}
