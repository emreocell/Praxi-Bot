import { ArrowLeft, Check, ChevronDown, House, Maximize2, Monitor, Plus, RefreshCw, Server, Square, Terminal } from "lucide-react";
import { useCallback, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { isMockMode } from "../api/mockMode";
import { humanLine } from "../lib/humanError";
import { homeStatus } from "../lib/home";
import { hostAcceptsWork } from "../lib/hostState";
import { useDaemon } from "../state/DaemonContext";
import { useLiveView } from "../state/LiveViewContext";
import { useServerTerminal } from "../state/ServerTerminalContext";
import styles from "./ComputerPicker.module.css";
import { AddServerModal } from "./AddServerModal";
import { setHostComputerAccess } from "./ComputerAccess";
import { CopySetupModal } from "./CopySetupModal";
import { PairHostModal } from "./PairHostModal";
import { Popover } from "./Popover";
import { useToast } from "./Toast";

/**
 * The status chip of the bots' computer. It is not a switch between two worlds: the app follows the
 * home (the host the owner adopted) by itself. The menu manages that computer - add a server, make
 * one the home, sync, look at its screen - and holds the one explicit, temporary exception,
 * "work on this computer for now", with a visible way back.
 */
export function ComputerPicker() {
  const { t } = useTranslation();
  const toast = useToast();
  const { home, workLocalNow, setWorkLocalNow, hostState, hostList, stopHostRuns, localApi, refreshHosts } = useDaemon();
  const { openViewer } = useLiveView();
  const serverTerminal = useServerTerminal();
  // Design review (mock mode only): `?menu=1` opens the menu.
  const [open, setOpen] = useState(() => isMockMode() && new URLSearchParams(window.location.search).get("menu") === "1");
  const [addOpen, setAddOpen] = useState(false);
  const [pairOpen, setPairOpen] = useState(false);
  const [syncHost, setSyncHost] = useState<string | null>(null);
  const [stopping, setStopping] = useState(false);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const close = useCallback(() => setOpen(false), []);

  const status = homeStatus(home, workLocalNow, hostState);
  const name = home.kind === "host" ? home.name || t("computerPicker.host") : "";
  const label = t(`home.chip.${status}`, { name });
  const tone = status === "online" || status === "local" ? "ok" : status === "connecting" || status === "temporary" ? "wait" : "bad";
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
        title={t("computerPicker.label", { name: label })}
        data-testid="computer-picker"
        data-computer={home.kind === "host" ? "host" : "local"}
        data-status={status}
        onClick={() => setOpen((o) => !o)}
      >
        <i className={styles.dot} data-tone={tone} aria-hidden="true" />
        <Icon aria-hidden="true" className={styles.icon} />
        <span className={styles.name}>{label}</span>
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
        <p className={styles.heading}>{t("home.menu.title")}</p>
        <div className={styles.item} data-testid="home-status" aria-live="polite">
          <i className={styles.dot} data-tone={tone} aria-hidden="true" />
          <Icon aria-hidden="true" className={styles.icon} />
          <span className={styles.itemText}>
            <b>{label}</b>
          </span>
        </div>
        {home.kind === "host" ? (
          <>
            {item("home-sync", <RefreshCw aria-hidden="true" className={styles.icon} />, t("home.menu.sync", { name }), () => setSyncHost(home.hostId))}
            {status === "online"
              ? item("home-view-screen", <Maximize2 aria-hidden="true" className={styles.icon} />, t("home.menu.viewScreen"), () => openViewer())
              : null}
            {serverTerminal.available && status === "online"
              ? item("home-terminal", <Terminal aria-hidden="true" className={styles.icon} />, t("home.menu.terminal"), () => serverTerminal.open())
              : null}
            {hostState && hostAcceptsWork(hostState) && !workLocalNow
              ? item("computer-stop-host", <Square aria-hidden="true" className={styles.icon} />, t("home.menu.stop", { name }), () => void stop(), {
                  hint: t("home.menu.stopHint"),
                  danger: true,
                  disabled: stopping
                })
              : null}
          </>
        ) : null}
        {unadopted.map((h) =>
          item(`home-make-${h.id}`, <House aria-hidden="true" className={styles.icon} />, t("home.menu.makeHome", { name: h.name }), () => setSyncHost(h.id))
        )}
        {item("computer-add", <Plus aria-hidden="true" className={styles.icon} />, t("home.menu.add"), () => setAddOpen(true))}
        {homeLink ? (
          <button
            type="button"
            role="menuitemcheckbox"
            aria-checked={pcAllowed}
            className={styles.item}
            data-testid="home-pc-access"
            onClick={() => {
              setOpen(false);
              togglePc();
            }}
          >
            <Monitor aria-hidden="true" className={styles.icon} />
            <span className={styles.itemText}>
              <b>{t("computerAccess.title")}</b>
              <small>{pcAllowed ? t("home.menu.pcOn") : t("home.menu.pcOff")}</small>
            </span>
            {pcAllowed ? <Check aria-hidden="true" className={styles.check} /> : null}
          </button>
        ) : null}
        {home.kind === "host"
          ? workLocalNow
            ? item("home-back", <ArrowLeft aria-hidden="true" className={styles.icon} />, t("home.menu.backHome", { name }), () => setWorkLocalNow(false), {
                hint: t("home.menu.backHomeHint")
              })
            : item("home-work-here", <Monitor aria-hidden="true" className={styles.icon} />, t("home.menu.workHere"), () => setWorkLocalNow(true), {
                hint: t("home.menu.workHereHint")
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
      <PairHostModal open={pairOpen} onClose={() => setPairOpen(false)} />
      <CopySetupModal open={syncHost !== null} hostId={syncHost ?? ""} onClose={() => setSyncHost(null)} />
    </>
  );
}
