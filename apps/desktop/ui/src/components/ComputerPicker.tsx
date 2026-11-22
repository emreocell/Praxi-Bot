import { ArrowLeft, ChevronDown, Eye, House, Monitor, Plus, RefreshCw, Server, Square, Terminal } from "lucide-react";
import { useCallback, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { isMockMode } from "../api/mockMode";
import { humanLine } from "../lib/humanError";
import { homeStatus } from "../lib/home";
import { hostAcceptsWork } from "../lib/hostState";
import { lastSyncParts, readLastSync } from "../lib/lastSync";
import { useDaemon } from "../state/DaemonContext";
import { useLiveView } from "../state/LiveViewContext";
import { useServerTerminal } from "../state/ServerTerminalContext";
import styles from "./ComputerPicker.module.css";
import { AddServerModal } from "./AddServerModal";
import { ServersModal } from "./ServersModal";
import { setHostComputerAccess } from "./ComputerAccess";
import { CopySetupModal } from "./CopySetupModal";
import { PairHostModal } from "./PairHostModal";
import { Popover } from "./Popover";
import { PeerLinkBadge } from "./PeerLinkBadge";
import { useToast } from "./Toast";

/**
 * The status chip of the bots' computer. It is not a switch between two worlds: the app follows the
 * home (the host the owner adopted) by itself. The menu manages that computer - look at its screen,
 * sync, add a server, make one the home - and holds the one explicit, temporary exception,
 * "work on this computer for now", with a visible way back. Only the items that apply are shown.
 */
export function ComputerPicker() {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const { home, workLocalNow, setWorkLocalNow, hostState, hostList, stopHostRuns, localApi, refreshHosts } = useDaemon();
  const { openViewer, viewerOpen } = useLiveView();
  const serverTerminal = useServerTerminal();
  // Design review (mock mode only): `?menu=1` opens the menu.
  const [open, setOpen] = useState(() => isMockMode() && new URLSearchParams(window.location.search).get("menu") === "1");
  const [addOpen, setAddOpen] = useState(false);
  const [pairOpen, setPairOpen] = useState(false);
  const [serversOpen, setServersOpen] = useState(false);
  const [syncHost, setSyncHost] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const linkId = useId();
  const close = useCallback(() => setOpen(false), []);

  const status = homeStatus(home, workLocalNow, hostState);
  const name = home.kind === "host" ? home.name || t("computerPicker.host") : "";
  const label = t(`home.chip.${status}`, { name });
  const tone = status === "online" || status === "local" ? "ok" : status === "connecting" || status === "starting" || status === "temporary" ? "wait" : "bad";
  const Icon = home.kind === "host" && status !== "temporary" ? Server : Monitor;
  const unadopted = (hostList?.hosts ?? []).filter((h) => h.id !== (home.kind === "host" ? home.hostId : ""));

  const homeLink = home.kind === "host" ? (hostList?.hosts.find((h) => h.id === home.hostId) ?? null) : null;
  const pcAllowed = homeLink?.computer_access === true;
  const togglePc = () => {
    if (!homeLink) {
      return;
    }
    void setHostComputerAccess(localApi, homeLink, !pcAllowed)
      .then(() => refreshHosts())
      .catch((e: unknown) => toast.show(humanLine(e), "danger"));
  };

  // The time the setup was last synced to this server (remembered by this app), once the menu opens.
  const syncedAt = open && home.kind === "host" ? readLastSync(home.hostId) : null;
  const syncWhen = (ms: number) => {
    const p = lastSyncParts(ms, Date.now(), i18n.language);
    return p.day ? `${p.day} ${p.time}` : t("homeMenu.today", { time: p.time });
  };

  const stop = async () => {
    setOpen(false);
    setStopping(true);
    try {
      const n = await stopHostRuns();
      toast.show(
        n === 0 ? t("computerPicker.stopNone") : t("computerPicker.stopped", { count: n }),
        n === 0 ? "info" : "success"
      );
    } catch {
      toast.show(t("computerPicker.stopFailed"), "danger");
    } finally {
      setStopping(false);
    }
  };

  const item = (
    testId: string,
    icon: React.ReactNode,
    title: string,
    onClick: () => void,
    extra?: { hint?: string; danger?: boolean; disabled?: boolean }
  ) => (
    <button
      type="button"
      role="menuitem"
      className={`${styles.item} ${extra?.danger ? styles.danger : ""}`}
      data-testid={testId}
      disabled={extra?.disabled}
      onClick={() => {
        setOpen(false);
        trigger.current?.focus();
        onClick();
      }}
    >
      {icon}
      <span className={styles.itemText}>
        <b>{title}</b>
        {extra?.hint ? <small>{extra.hint}</small> : null}
      </span>
    </button>
  );

  return (
    <>
      <button
        ref={trigger}
        type="button"
        className={styles.trigger}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={t("computerPicker.label", { name: label })}
        aria-describedby={linkId}
        title={t("computerPicker.label", { name: label })}
        data-testid="computer-picker"
        data-computer={home.kind === "host" ? "host" : "local"}
        data-status={status}
        onClick={() => setOpen((o) => !o)}
      >
        <i className={styles.dot} data-tone={tone} aria-hidden="true" />
        <Icon aria-hidden="true" className={styles.icon} />
        <span className={styles.name}>{label}</span>
        <PeerLinkBadge enabled={status === "online" && !viewerOpen} id={linkId} />
        <ChevronDown aria-hidden="true" className={styles.chevron} />
      </button>
      <Popover
        open={open}
        anchor={trigger}
        onClose={close}
        placement="bottom-end"
        label={t("home.menu.title")}
        width={300}
        className={styles.menu}
      >
        <div className={styles.head} data-testid="home-status" aria-live="polite">
          <Icon aria-hidden="true" className={styles.icon} />
          <div className={styles.headText}>
            <b>{home.kind === "host" ? name : t("account.localComputer")}</b>
            <small>
              <i className={styles.dot} data-tone={tone} aria-hidden="true" />
              {t(`homeMenu.lives.${status}`, { name })}
            </small>
          </div>
        </div>
        {home.kind === "host" && !workLocalNow ? (
          <>
            {status === "online"
              ? item("home-view-screen", <Eye aria-hidden="true" className={styles.icon} />, t("home.menu.viewScreen"), () => openViewer())
              : null}
            {serverTerminal.available && status === "online"
              ? item("home-terminal", <Terminal aria-hidden="true" className={styles.icon} />, t("home.menu.terminal"), () => serverTerminal.open())
              : null}
          </>
        ) : null}
        {home.kind === "host"
          ? item("home-sync", <RefreshCw aria-hidden="true" className={styles.icon} />, t("homeMenu.sync"), () => setSyncHost(home.hostId), {
              hint: syncedAt === null ? undefined : t("homeMenu.lastSync", { when: syncWhen(syncedAt) })
            })
          : null}
        {homeLink ? (
          <>
            <div className={styles.hr} role="separator" />
            <button
              type="button"
              role="menuitemcheckbox"
              aria-checked={pcAllowed}
              className={styles.item}
              data-testid="home-pc-access"
              onClick={togglePc}
            >
              <Monitor aria-hidden="true" className={styles.icon} />
              <span className={styles.itemText}>
                <b>{t("computerAccess.title")}</b>
                <small>{pcAllowed ? t("homeMenu.pcOn") : t("homeMenu.pcOff")}</small>
              </span>
              <span className={styles.tog} data-on={pcAllowed ? "true" : undefined} aria-hidden="true" />
            </button>
          </>
        ) : null}
        <div className={styles.hr} role="separator" />
        {item("computer-add", <Plus aria-hidden="true" className={styles.icon} />, t("home.menu.add"), () => setAddOpen(true))}
        {item("computer-servers", <Server aria-hidden="true" className={styles.icon} />, t("servers.title"), () => setServersOpen(true))}
        {unadopted.map((h) =>
          item(`home-make-${h.id}`, <House aria-hidden="true" className={styles.icon} />, t("home.menu.makeHome", { name: h.name }), () => setSyncHost(h.id))
        )}
        {home.kind === "host"
          ? workLocalNow
            ? item("home-back", <ArrowLeft aria-hidden="true" className={styles.icon} />, t("home.menu.backHome", { name }), () => setWorkLocalNow(false), {
                hint: t("home.menu.backHomeHint")
              })
            : item("home-work-here", <Monitor aria-hidden="true" className={styles.icon} />, t("home.menu.workHere"), () => setWorkLocalNow(true))
          : null}
        {home.kind === "host" && hostState && hostAcceptsWork(hostState) && !workLocalNow
          ? item("computer-stop-host", <Square aria-hidden="true" className={styles.icon} />, t("home.menu.stop", { name }), () => void stop(), {
              danger: true,
              disabled: stopping
            })
          : null}
      </Popover>
      <AddServerModal
        open={addOpen}
        onClose={() => setAddOpen(false)}
        onManual={() => {
          setAddOpen(false);
          setPairOpen(true);
        }}
      />
      <ServersModal
        open={serversOpen}
        onClose={() => setServersOpen(false)}
        onAdd={() => {
          setServersOpen(false);
          setAddOpen(true);
        }}
      />
      <PairHostModal open={pairOpen} onClose={() => setPairOpen(false)} />
      <CopySetupModal open={syncHost !== null} hostId={syncHost ?? ""} onClose={() => setSyncHost(null)} />
    </>
  );
}
