import { Check, ChevronDown, Monitor, Server } from "lucide-react";
import { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Bot } from "../api/types";
import { botWriteMessage, writeBotProfile } from "../lib/botWrite";
import { useDaemon } from "../state/DaemonContext";
import { Popover } from "./Popover";
import { useToast } from "./Toast";
import styles from "./ComputerTargetChip.module.css";

type Json = Record<string, unknown>;

/** The bot's `runtime.computer` as the chip shows it: its own computer (`auto`, `host`) or mine (`pc`). */
export function computerTargetOf(bot: Pick<Bot, "config">): "bot" | "mine" {
  const runtime = (bot.config as Json | null)?.runtime as Json | undefined;
  return runtime?.computer === "pc" ? "mine" : "bot";
}

/**
 * "Runs on: <server name> / This computer" for the open chat's bot. A small menu with the two places
 * in plain words, each with one line on what it means; picking one saves the bot's own
 * `runtime.computer` through the versioned bot write. Shown only while the bots' home is a server
 * (without one there is only this computer).
 */
export function ComputerTargetChip({ bot }: { bot: Bot }) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, home, workLocalNow, refreshBots } = useDaemon();
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const close = useCallback(() => setOpen(false), []);
  if (home.kind !== "host" || workLocalNow || !client) {
    return null;
  }
  const value = computerTargetOf(bot);
  const server = home.name.trim() || t("home.target.server");
  const where = value === "bot" ? server : t("home.target.mine");
  const choose = async (next: "bot" | "mine") => {
    setOpen(false);
    trigger.current?.focus();
    if (next === value) return;
    setBusy(true);
    try {
      await writeBotProfile(client, bot, (current) => {
        const config = { ...((current.config as Json | null) ?? {}) };
        config.runtime = { ...((config.runtime as Json | undefined) ?? {}), computer: next === "mine" ? "pc" : "host" };
        return { name: current.name, config } as never;
      });
      await refreshBots();
    } catch (e) {
      toast.show(botWriteMessage(e, t), "danger");
    } finally {
      setBusy(false);
    }
  };
  const Icon = value === "bot" ? Server : Monitor;
  const options = [
    { id: "bot" as const, Icon: Server, name: server, detail: t("home.target.botDetail") },
    { id: "mine" as const, Icon: Monitor, name: t("home.target.mine"), detail: t("home.target.mineDetail") }
  ];
  return (
    <>
      <button
        ref={trigger}
        type="button"
        className={styles.chip}
        disabled={busy}
        title={open ? undefined : t("home.target.title")}
        aria-haspopup="menu"
        aria-expanded={open}
        data-testid="computer-target-chip"
        data-target={value}
        onClick={() => setOpen((o) => !o)}
      >
        <Icon aria-hidden="true" />
        <span className={styles.label}>{t("home.target.label")} </span>
        <b>{where}</b>
        <ChevronDown aria-hidden="true" className={styles.chev} />
      </button>
      <Popover
        open={open}
        anchor={trigger}
        onClose={close}
        placement="top-start"
        label={t("home.target.title")}
        className={styles.menu}
      >
        <p className={styles.head} aria-hidden="true">{t("home.target.title")}</p>
        {options.map((o) => (
          <button
            key={o.id}
            type="button"
            role="menuitemradio"
            aria-checked={o.id === value}
            className={styles.item}
            data-testid={`computer-target-${o.id}`}
            onClick={() => void choose(o.id)}
          >
            <span className={styles.icon} aria-hidden="true">
              <o.Icon />
            </span>
            <span className={styles.copy}>
              <span className={styles.name}>{o.name}</span>
              <span className={styles.detail}>{o.detail}</span>
            </span>
            {o.id === value ? <Check aria-hidden="true" className={styles.check} /> : null}
          </button>
        ))}
      </Popover>
    </>
  );
}
