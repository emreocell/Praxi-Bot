import { ChevronDown, Eye, Monitor, Plus, RefreshCw, Server, Square, Terminal } from "lucide-react";
import { useCallback, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { isMockMode } from "../api/mockMode";
import { humanLine } from "../lib/humanError";
import { homeStatus } from "../lib/home";
import { hostAcceptsWork, liveRunIds } from "../lib/hostState";
import { lastSyncParts, readLastSync } from "../lib/lastSync";
import { type RunPlace, stopResult, switchNeedsConfirm } from "../lib/runLocation";
import { useDaemon } from "../state/DaemonContext";
import { useLiveView } from "../state/LiveViewContext";
import { useServerTerminal } from "../state/ServerTerminalContext";
import styles from "./ComputerPicker.module.css";
import { AddServerModal } from "./AddServerModal";
import { ServersModal } from "./ServersModal";
import { setHostComputerAccess } from "./ComputerAccess";
import { CopySetupModal } from "./CopySetupModal";
import { ConfirmDialog } from "./DialogParts";
import { PairHostModal } from "./PairHostModal";
import { Popover } from "./Popover";
import { PeerLinkBadge } from "./PeerLinkBadge";
import { useToast } from "./Toast";

/**
 * "Where do my bots run?" Two first-class choices: this computer (no server needed) and the server
 * the owner added (always on). The pill is short (a status dot and the place); the popover lists the
 * places as radio rows, what can be done there, "Add a server" as a secondary action and, apart from
 * the rest, Stop for everything running on the current place. Switching while something runs asks
 * first. Only the rows that apply are shown.
 */
export function ComputerPicker() {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const { home, workLocalNow, setWorkLocalNow, hostState, hostList, stopHostRuns, localApi, refreshHosts, runs } = useDaemon();
  const { openViewer, viewerOpen } = useLiveView();
  const serverTerminal = useServerTerminal();
  // Design review (mock mode only): `?menu=1` opens the menu.
  const [open, setOpen] = useState(() => isMockMode() && new URLSearchParams(window.location.search).get("menu") === "1");
  const [addOpen, setAddOpen] = useState(false);
  const [pairOpen, setPairOpen] = useState(false);
  const [serversOpen, setServersOpen] = useState(false);
  const [syncHost, setSyncHost] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const [confirm, setConfirm] = useState<null | { kind: "switch"; to: RunPlace } | { kind: "stop" }>(null);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const group = useRef<HTMLDivElement | null>(null);
  const linkId = useId();
  const close = useCallback(() => setOpen(false), []);

  const hasServer = home.kind === "host";
  const place: RunPlace = hasServer && !workLocalNow ? "server" : "this";
  const serverName = home.kind === "host" ? home.name || t("computerPicker.host") : "";
  const placeName = place === "server" ? serverName : t("runLocation.thisComputer");
  const status = homeStatus(home, workLocalNow, hostState);
  const serverStatus = homeStatus(home, false, hostState);
  const toneOf = (s: string) => (s === "online" || s === "local" ? "ok" : s === "connecting" || s === "starting" ? "wait" : "bad");
  const tone = place === "this" ? "ok" : toneOf(status);
  const label = place === "this" ? t("runLocation.thisComputer") : t(`home.chip.${status}`, { name: serverName });
  const Icon = place === "server" ? Server : Monitor;
  const unadopted = (hostList?.hosts ?? []).filter((h) => h.id !== (home.kind === "host" ? home.hostId : ""));
  const liveCount = liveRunIds(runs).length;
  const canStop = place === "this" || (hostState !== null && hostAcceptsWork(hostState));

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

  const nameOf = (p: RunPlace) => (p === "server" ? serverName : t("runLocation.thisComputer"));

  const choose = (to: RunPlace) => {
    if (to === place) {
      setOpen(false);
      return;
    }
    if (switchNeedsConfirm(liveCount)) {
      setConfirm({ kind: "switch", to });
      return;
    }
    setOpen(false);
    setWorkLocalNow(to === "this");
  };

  const stop = async () => {
    setConfirm(null);
    setOpen(false);
    setStopping(true);
    const where = placeName;
    try {
      const n = await stopHostRuns();
      const r = stopResult(n);
      toast.show(t(r.key, { count: n, name: where }), r.tone);
    } catch {
      toast.show(t("runLocation.stopFailed", { name: where }), "danger");
    } finally {
      setStopping(false);
    }
  };

  // Radio rows: arrows move the focus between them, Enter / Space choose.
  const onGroupKey = (e: React.KeyboardEvent) => {
    if (!["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight"].includes(e.key)) {
      return;
    }
    const rows = [...(group.current?.querySelectorAll<HTMLElement>('[role="radio"]') ?? [])];
    const at = rows.indexOf(document.activeElement as HTMLElement);
    if (rows.length === 0 || at < 0) {
      return;
    }
    e.preventDefault();
    const step = e.key === "ArrowDown" || e.key === "ArrowRight" ? 1 : -1;
    rows[(at + step + rows.length) % rows.length]?.focus();
  };

  const radio = (testId: string, p: RunPlace, icon: React.ReactNode, title: string, hint: string, dotTone?: string) => (
    <button
      type="button"
      role="radio"
      aria-checked={place === p}
      tabIndex={place === p ? 0 : -1}
      className={styles.radio}
      data-testid={testId}
      onClick={() => choose(p)}
    >
      {icon}
      <span className={styles.itemText}>
        <b>{title}</b>
        <small>
          {dotTone ? <i className={styles.dot} data-tone={dotTone} aria-hidden="true" /> : null}
          {hint}
        </small>
      </span>
      <span className={styles.mark} aria-hidden="true" />
    </button>
  );

  const item = (
    testId: string,
    icon: React.ReactNode,
    title: string,
    onClick: () => void,
    extra?: { hint?: string; danger?: boolean; disabled?: boolean }
  ) => (
    <button
      type="button"
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

  const confirmTo = confirm?.kind === "switch" ? nameOf(confirm.to) : "";
  const serverWord = serverStatus === "local" || serverStatus === "temporary" ? "offline" : serverStatus;

  return (
    <>
      <button
        ref={trigger}
        type="button"
        className={styles.trigger}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={t("computerPicker.label", { name: label })}
        aria-describedby={linkId}
        title={t("computerPicker.label", { name: label })}
        data-testid="computer-picker"
        data-computer={place === "server" ? "host" : "local"}
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
        role="dialog"
        label={t("runLocation.title")}
        width={320}
        className={styles.menu}
      >
        <div className={styles.head} data-testid="home-status" aria-live="polite">
          <b>{t("runLocation.title")}</b>
          <small>
            <i className={styles.dot} data-tone={tone} aria-hidden="true" />
            {t(`runLocation.status.${place === "this" ? "local" : status}`, { name: placeName })}
          </small>
        </div>
        <div role="radiogroup" aria-label={t("runLocation.title")} ref={group} onKeyDown={onGroupKey} className={styles.group}>
          {radio(
            "run-place-this",
            "this",
            <Monitor aria-hidden="true" className={styles.icon} />,
            t("runLocation.thisComputer"),
            t("runLocation.thisComputerHint")
          )}
          {hasServer
            ? radio(
                "run-place-server",
                "server",
                <Server aria-hidden="true" className={styles.icon} />,
                serverName,
                `${t("runLocation.serverHint")} · ${t(`runLocation.serverState.${serverWord}`)}`,
                toneOf(serverStatus)
              )
            : null}
        </div>
        {place === "server" && home.kind === "host" ? (
          <>
            <div className={styles.hr} role="separator" />
            <div className={styles.section}>{t("runLocation.on", { name: serverName })}</div>
            {status === "online"
              ? item("home-view-screen", <Eye aria-hidden="true" className={styles.icon} />, t("home.menu.viewScreen"), () => openViewer())
              : null}
            {serverTerminal.available && status === "online"
              ? item("home-terminal", <Terminal aria-hidden="true" className={styles.icon} />, t("home.menu.terminal"), () => serverTerminal.open())
              : null}
            {item("home-sync", <RefreshCw aria-hidden="true" className={styles.icon} />, t("homeMenu.sync"), () => setSyncHost(home.hostId), {
              hint: syncedAt === null ? undefined : t("homeMenu.lastSync", { when: syncWhen(syncedAt) })
            })}
            {homeLink ? (
              <button
                type="button"
                role="switch"
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
            ) : null}
          </>
        ) : null}
        <div className={styles.hr} role="separator" />
        {item("computer-add", <Plus aria-hidden="true" className={styles.icon} />, t("home.menu.add"), () => setAddOpen(true), {
          hint: hasServer ? undefined : t("runLocation.addHint")
        })}
        {item("computer-servers", <Server aria-hidden="true" className={styles.icon} />, t("servers.title"), () => setServersOpen(true))}
        {unadopted.map((h) =>
          item(`home-make-${h.id}`, <Server aria-hidden="true" className={styles.icon} />, t("home.menu.makeHome", { name: h.name }), () => setSyncHost(h.id))
        )}
        {canStop ? (
          <>
            <div className={styles.hr} role="separator" />
            <div className={styles.zone} data-testid="run-danger-zone">
              <button
                type="button"
                className={`${styles.item} ${styles.danger}`}
                data-testid="computer-stop-host"
                disabled={stopping || liveCount === 0}
                onClick={() => setConfirm({ kind: "stop" })}
              >
                <Square aria-hidden="true" className={styles.icon} />
                <span className={styles.itemText}>
                  <b>{t("runLocation.stopAll", { name: placeName })}</b>
                  <small>{liveCount === 0 ? t("runLocation.stopIdle") : t("runLocation.stopHint", { count: liveCount })}</small>
                </span>
              </button>
            </div>
          </>
        ) : null}
      </Popover>
      <ConfirmDialog
        open={confirm?.kind === "switch"}
        title={t("runLocation.switchTitle", { to: confirmTo })}
        body={t("runLocation.switchBody", { count: liveCount, from: placeName, to: confirmTo })}
        confirmLabel={t("runLocation.switchConfirm", { to: confirmTo })}
        onCancel={() => setConfirm(null)}
        onConfirm={() => {
          const to = confirm?.kind === "switch" ? confirm.to : place;
          setConfirm(null);
          setOpen(false);
          setWorkLocalNow(to === "this");
        }}
      />
      <ConfirmDialog
        open={confirm?.kind === "stop"}
        title={t("runLocation.stopTitle", { name: placeName })}
        body={t("runLocation.stopBody", { count: liveCount, name: placeName })}
        confirmLabel={t("runLocation.stopConfirm", { count: liveCount })}
        tone="danger"
        onCancel={() => setConfirm(null)}
        onConfirm={() => void stop()}
      />
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
