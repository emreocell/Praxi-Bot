import { invoke } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { switchesOf, type ApiClient, type ComputerSwitches } from "../api/client";
import type { PcRequestInfo } from "../api/types";
import { SwitchQueue } from "../lib/computerSwitchQueue";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import { Modal } from "./Modal";
import { Monitor, ServerOff } from "lucide-react";
import { EmptyNote, SettingRow, Switch } from "./SettingsUi";
import styles from "./ComputerAccess.module.css";

/** The owner key of this app's daemon (Tauri command, main window only), sent on owner-only routes. */
export const ownerKey = () => invoke<string>("owner_key");

/**
 * Turn "let this server's bots use this computer" (screen and input) on or off through the same
 * owner-key route as the Settings switch (`setComputerAccess`). The other families keep their state.
 */
export async function setHostComputerAccess(
  api: Pick<ApiClient, "setComputerAccess"> | null,
  host: Parameters<typeof switchesOf>[0] & { id: string },
  enabled: boolean
): Promise<void> {
  if (!api) {
    throw new Error("local_daemon_unavailable");
  }
  const now = switchesOf(host);
  await api.setComputerAccess(host.id, { ...now, screen: enabled, alwaysRead: enabled && now.alwaysRead }, await ownerKey());
}

/** Shown in the chat that selected this PC while its paired host lacks screen access. */
export function ComputerUseOffCard() {
  const { t } = useTranslation();
  const { home, workLocalNow, hostList, localApi, refreshHosts } = useDaemon();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const host = home.kind === "host" ? hostList?.hosts.find((h) => h.id === home.hostId) : undefined;
  if (workLocalNow || !host || host.computer_access === true) return null;
  const enable = async () => {
    setBusy(true);
    setError(null);
    try {
      await setHostComputerAccess(localApi, host, true);
      await refreshHosts();
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <section className={styles.enable} aria-label={t("computerAccess.enableTitle")} data-testid="computer-use-off">
      <Monitor className={styles.enableIcon} aria-hidden="true" />
      <div className={styles.stack}>
        <strong>{t("computerAccess.enableTitle")}</strong>
        <p className={styles.hint}>{t("computerAccess.enableHint", { name: host.name })}</p>
        {error ? <p className={styles.error} role="alert">{error}</p> : null}
      </div>
      <Button variant="primary" size="sm" disabled={busy || !localApi} onClick={() => void enable()}>
        {t("computerAccess.enableAction")}
      </Button>
    </section>
  );
}

type Family = "screen" | "browser" | "commands" | "files";
const FAMILIES: Family[] = ["screen", "browser", "commands", "files"];

/**
 * Settings > Computer: what each paired server's bots may do on this computer, one switch per
 * family (screen and input, browser, commands, files). All off by default; only the person at
 * this computer can turn them on (the daemon refuses every paired device on these routes).
 */
export function ComputerAccessCard() {
  const { t } = useTranslation();
  const { hostList, localApi, refreshHosts } = useDaemon();
  const [error, setError] = useState<string | null>(null);
  const [, redraw] = useState(0);
  // Changes go out one after the other, each built from the last state sent, so two quick
  // toggles never resend a stale switch set (the latest props lag behind).
  const io = useRef({ localApi, refreshHosts });
  io.current = { localApi, refreshHosts };
  const queue = useRef<SwitchQueue | null>(null);
  queue.current ??= new SwitchQueue(
    async (id, next) => {
      const key = await ownerKey();
      await io.current.localApi?.setComputerAccess(id, next, key);
      await io.current.refreshHosts();
    },
    (e: unknown) => setError(humanLine(e)),
    () => redraw((n) => n + 1)
  );
  const hosts = hostList?.hosts ?? [];
  if (hosts.length === 0) {
    return <EmptyNote icon={<ServerOff />}>{t("computerAccess.noServer")}</EmptyNote>;
  }
  const change = (h: (typeof hosts)[number], patch: Partial<ComputerSwitches>) => {
    setError(null);
    void queue.current?.change(h.id, switchesOf(h), patch);
  };
  return (
    <div className={styles.stack}>
      <p className={styles.hint}>{t("computerAccess.explain")}</p>
      {hosts.map((h) => {
        const now = queue.current?.view(h.id, switchesOf(h)) ?? switchesOf(h);
        return FAMILIES.map((family) => (
          <SettingRow
            key={`${h.id}-${family}`}
            title={t(`computerAccess.family.${family}`, { name: h.name })}
            description={t(`computerAccess.familyNote.${family}`)}
          >
            <Switch
              checked={now[family]}
              label={t(`computerAccess.family.${family}`, { name: h.name })}
              onChange={(on) =>
                change(h, family === "screen" ? { screen: on, ...(on ? {} : { alwaysRead: false }) } : { [family]: on })
              }
            />
          </SettingRow>
        ));
      })}
      {hosts
        .filter((h) => h.computer_access === true)
        .map((h) => (
          <SettingRow
            key={`${h.id}-always`}
            title={t("computerAccess.alwaysRead", { name: h.name })}
            description={t("computerAccess.alwaysReadNote")}
          >
            <Switch
              checked={(queue.current?.view(h.id, switchesOf(h)) ?? switchesOf(h)).alwaysRead}
              label={t("computerAccess.alwaysRead", { name: h.name })}
              onChange={(always) => change(h, { alwaysRead: always })}
            />
          </SettingRow>
        ))}
      {error ? (
        <p className={styles.error} role="alert">
          {error}
        </p>
      ) : null}
    </div>
  );
}

/** Polls this computer's daemon for a server asking to act here; the owner answers. */
export function ComputerRequestModal() {
  const { t } = useTranslation();
  const { hostList, localApi } = useDaemon();
  const [pending, setPending] = useState<PcRequestInfo[]>([]);
  const [reading, setReading] = useState<string[]>([]);
  const watching = (hostList?.hosts ?? []).some(
    (h) => h.computer_access || h.computer_browser || h.computer_commands || h.computer_files
  );
  useEffect(() => {
    if (!watching || !localApi) {
      setPending([]);
      setReading([]);
      return undefined;
    }
    let stopped = false;
    const tick = () => {
      ownerKey()
        .then((key) => localApi.computerRequests(key))
        .then((list) => {
          if (stopped) return;
          setPending(list.requests);
          setReading(list.reading);
        })
        .catch(() => {});
    };
    tick();
    const id = window.setInterval(tick, 1500);
    return () => {
      stopped = true;
      window.clearInterval(id);
    };
  }, [watching, localApi]);
  const current = pending[0];
  const answer = (allow: boolean) => {
    if (!current || !localApi) return;
    setPending((p) => p.filter((r) => r.id !== current.id));
    void ownerKey()
      .then((key) => localApi.decideComputerRequest(current.id, allow, key))
      .catch(() => {});
  };
  return (
    <>
      {reading.length > 0 ? (
        <div className={styles.looking} role="status" data-testid="computer-looking">
          <i aria-hidden="true" />
          {t("computerAccess.looking", { name: reading.join(", ") })}
        </div>
      ) : null}
      <Modal
        open={Boolean(current)}
        onClose={() => answer(false)}
        label={t("computerAccess.requestTitle")}
        size="sm"
      >
        {current ? (
          <div className={styles.request} data-testid="computer-request">
            <h2 className={styles.title}>
              {current.kind === "session_read"
                ? t("computerAccess.readTitle")
                : t("computerAccess.requestTitle")}
            </h2>
            <p className={styles.hint}>
              {t("computerAccess.requestBody")}{" "}
              <span className={styles.untrusted} title={t("computerAccess.fromServer")}>
                {t("computerAccess.fromServerName", { name: current.host_name })}
              </span>
              {current.bot_name ? (
                <>
                  {" "}
                  <span className={styles.untrusted} title={t("computerAccess.fromServer")} data-testid="computer-request-bot">
                    {t("computerAccess.fromBotName", { name: current.bot_name })}
                  </span>
                </>
              ) : null}
            </p>
            <dl className={styles.facts}>
              <dt>{t("computerAccess.what")}</dt>
              <dd>{current.action}</dd>
              <dt>{t("computerAccess.where")}</dt>
              <dd>{current.target}</dd>
              <dt>{t("computerAccess.details")}</dt>
              <dd className={styles.payload}>{current.payload}</dd>
            </dl>
            <div className={styles.actions}>
              <Button variant="ghost" onClick={() => answer(false)}>
                {t("computerAccess.deny")}
              </Button>
              <Button variant="primary" onClick={() => answer(true)}>
                {current.kind === "session_read"
                  ? t("computerAccess.allowSession")
                  : t("computerAccess.allowOnce")}
              </Button>
            </div>
          </div>
        ) : null}
      </Modal>
    </>
  );
}
