import { Check, ChevronDown } from "lucide-react";
import { type ReactNode, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ComposioApp, ComposioToolkit } from "../api/types";
import { AppLogo } from "../components/AppLogo";
import { BotAccessPicker } from "../components/BotAccessPicker";
import { Skeleton, SkeletonLines } from "../components/Skeleton";
import { type AppTestState, isTestable } from "../lib/appTest";
import { GROUPS, composioTileState, tileAction, type TileState } from "../lib/appTiles";
import { type BotScope, botScope, botSelection } from "../lib/mcpApps";
import { useDaemon } from "../state/DaemonContext";
import styles from "./AppsModal.module.css";
import own from "./AppsGrid.module.css";

function tone(state: ComposioApp["state"]): "ok" | "busy" | "bad" | "muted" {
  return state === "active" ? "ok" : state === "pending" ? "busy" : state === "expired" || state === "failed" ? "bad" : "muted";
}

/** Search-independent filter chips: All and the six groups. */
export function GroupChips({ value, onChange }: { value: string; onChange: (g: string) => void }) {
  const { t } = useTranslation();
  return (
    <div className={own.chips} role="group" aria-label={t("apps.group.label")}>
      {["all", ...GROUPS].map((g) => (
        <button
          key={g}
          type="button"
          className={`${own.chip} ${value === g ? own.chipOn : ""}`}
          aria-pressed={value === g}
          data-testid={`apps-chip-${g}`}
          onClick={() => onChange(g)}
        >
          {t(`apps.group.${g}`)}
        </button>
      ))}
    </div>
  );
}

/** A tile of the grid: brand mark, name, one line, and one button (or the state). */
export function AppTile({
  name,
  description,
  state,
  testId,
  slug,
  logo,
  sheet,
  onConnect,
  onCancel
}: {
  name: string;
  description: string;
  state: TileState;
  testId: string;
  /** Composio toolkit slug (its logo is fetched by the daemon when Praxi ships no mark). */
  slug?: string;
  /** What the logo is looked up by when it differs from the shown name (a catalog icon id). */
  logo?: string;
  /** The connect sheet anchored to this tile while Connect runs. */
  sheet?: ReactNode;
  onConnect: () => void;
  /** Stops a pending sign-in (shown next to the waiting state). */
  onCancel?: () => void;
}) {
  const { t } = useTranslation();
  const action = tileAction(state);
  return (
    <li className={`${styles.cell} ${own.anchor} ${sheet ? styles.cellOpen : ""}`} data-testid={testId} data-state={state}>
      <div className={styles.cellRow}>
        <AppLogo name={logo ?? name} slug={slug} size={26} />
        <div className={styles.cellText}>
          <b className={styles.cellName}>
            <span className={styles.cellTitle}>{name}</span>
          </b>
          <small className={styles.cellSub} title={description}>
            {state === "failed"
              ? t("apps.composio.failedHint")
              : state === "expired"
                ? t("apps.composio.state.expired")
                : description}
          </small>
        </div>
        {action ? (
          <button
            type="button"
            className={styles.connect}
            aria-label={t("apps.connectApp", { name })}
            onClick={onConnect}
          >
            {action === "connect" ? t("apps.connect") : action === "retry" ? t("apps.composio.retry") : t("apps.reconnect")}
          </button>
        ) : (
          <>
            <span className={`${styles.state} ${styles.tone_busy}`} role="status">
              <span className={styles.dot} aria-hidden="true" />
              {t("apps.composio.waiting")}
            </span>
            {onCancel && (state === "waiting" || state === "linking") ? (
              <button type="button" className={styles.connect} data-testid={`${testId}-cancel`} onClick={onCancel}>
                {t("common.cancel")}
              </button>
            ) : null}
          </>
        )}
      </div>
      {sheet}
    </li>
  );
}

/** Skeleton tiles for the very first load only. */
export function SkeletonTiles({ count = 8 }: { count?: number }) {
  const { t } = useTranslation();
  return (
    <ul className={styles.grid} aria-busy="true" aria-label={t("apps.composio.loading")}>
      {Array.from({ length: count }, (_, i) => (
        <li key={i} className={styles.cell} aria-hidden="true">
          <div className={styles.cellRow}>
            <Skeleton shape="circle" width="var(--size-avatar-md)" height="var(--size-avatar-md)" />
            <div className={styles.cellText}>
              <SkeletonLines count={2} />
            </div>
          </div>
        </li>
      ))}
    </ul>
  );
}

/** A Composio app in the Connected section: state, and a small Manage menu. */
export function ComposioCell({
  app,
  busy,
  test,
  onEnabled,
  onReconnect,
  onDisconnect,
  onTest,
  onSaveBots
}: {
  app: ComposioApp;
  busy: boolean;
  /** What the last test of this app found, when one ran. */
  test?: AppTestState;
  onEnabled: (enabled: boolean) => void;
  onReconnect: () => void;
  onDisconnect: () => void;
  onTest: () => void;
  onSaveBots: (bots: string[] | null) => void;
}) {
  const { t } = useTranslation();
  const { bots } = useDaemon();
  const [open, setOpen] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [scope, setScope] = useState<BotScope>(() => botScope(app));
  const account = test?.status === "ok" ? test.account : null;
  const k = tone(app.state);
  const broken = app.state === "failed" || app.state === "expired";
  const panelId = `composio-manage-${app.slug}`;
  return (
    <li className={`${styles.cell} ${open ? styles.cellOpen : ""}`} data-testid={`composio-app-${app.slug}`} data-state={app.state}>
      <div className={styles.cellRow}>
        <AppLogo name={app.name} slug={app.slug} size={26} />
        <div className={styles.cellText}>
          <b className={styles.cellName}>
            <span className={styles.cellTitle}>{app.name}</span>
          </b>
          <small className={styles.cellSub}>
            {app.state === "pending"
              ? t("apps.composio.waiting")
              : app.state === "failed"
                ? t("apps.composio.failedHint")
                : app.state === "expired"
                  ? t("apps.composio.expiredHint")
                  : account
                    ? t("apps.connectedAs", { account })
                    : t("apps.toolCount", { count: app.tools })}
          </small>
        </div>
        <span className={`${styles.state} ${styles[`tone_${k}`]}`}>
          {k === "ok" ? <Check aria-hidden="true" /> : <span className={styles.dot} aria-hidden="true" />}
          {t(`apps.composio.state.${app.state}`)}
        </span>
        {broken ? (
          <Button size="sm" variant="secondary" disabled={busy} onClick={onReconnect}>
            {app.state === "failed" ? t("apps.composio.retry") : t("apps.reconnect")}
          </Button>
        ) : null}
        <button
          type="button"
          className={`${styles.manage} ${broken || app.state === "pending" ? styles.manageShown : ""}`}
          aria-expanded={open}
          aria-controls={panelId}
          data-testid={`composio-manage-${app.slug}`}
          onClick={() => {
            if (!open) {
              setScope(botSelection(botScope(app), bots));
            }
            setConfirm(false);
            setOpen((o) => !o);
          }}
        >
          {t("apps.manage")}
          <ChevronDown aria-hidden="true" />
        </button>
      </div>
      {open ? (
        <div className={styles.panel} id={panelId}>
          {app.state === "active" ? (
            <>
              <span className={styles.panelLabel}>{t("apps.access.label")}</span>
              <BotAccessPicker scope={scope} bots={bots} onChange={setScope} />
              <div className={styles.actions}>
                <Button
                  size="sm"
                  variant="primary"
                  disabled={busy}
                  data-testid={`composio-access-save-${app.slug}`}
                  onClick={() => onSaveBots(scope.mode === "all" ? null : [...scope.ids])}
                >
                  {t("apps.access.save")}
                </Button>
              </div>
              {isTestable(app.slug) ? <ComposioTestResult slug={app.slug} test={test} busy={busy} onTest={onTest} /> : null}
            </>
          ) : null}
          <div className={styles.actions}>
            {app.state === "active" ? (
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                aria-pressed={app.enabled}
                data-testid={`composio-enabled-${app.slug}`}
                onClick={() => onEnabled(!app.enabled)}
              >
                {app.enabled ? t("apps.composio.disable") : t("apps.composio.enable")}
              </Button>
            ) : null}
            <span className={styles.grow} />
            {confirm ? (
              <>
                <Button
                  size="sm"
                  variant="danger"
                  disabled={busy}
                  data-testid={`composio-disconnect-confirm-${app.slug}`}
                  onClick={() => {
                    setConfirm(false);
                    setOpen(false);
                    onDisconnect();
                  }}
                >
                  {t("apps.disconnectConfirm")}
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setConfirm(false)}>
                  {t("common.cancel")}
                </Button>
              </>
            ) : (
              <Button
                size="sm"
                variant="ghost"
                disabled={busy}
                data-testid={`composio-disconnect-${app.slug}`}
                onClick={() => setConfirm(true)}
              >
                {t("apps.disconnect")}
              </Button>
            )}
          </div>
        </div>
      ) : null}
    </li>
  );
}

/** The "Test connection" row of a connected app: the button, and what the last test found. */
function ComposioTestResult({
  slug,
  test,
  busy,
  onTest
}: {
  slug: string;
  test?: AppTestState;
  busy: boolean;
  onTest: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className={own.testBox} data-testid={`composio-test-${slug}`} data-status={test?.status ?? "idle"}>
      <div className={styles.actions}>
        <Button
          size="sm"
          variant="secondary"
          disabled={busy || test?.status === "running"}
          data-testid={`composio-test-run-${slug}`}
          onClick={onTest}
        >
          {test?.status === "running" ? t("apps.test.running") : t("apps.test.run")}
        </Button>
      </div>
      {test?.status === "ok" ? (
        <div role="status">
          <p className={own.testLine}>
            {test.account ? t("apps.test.okAs", { account: test.account }) : t("apps.test.ok")}
          </p>
          {test.items.length > 0 ? (
            <ul className={own.testItems} aria-label={t("apps.test.samples")}>
              {test.items.map((item, i) => (
                <li key={`${i}-${item}`}>{item}</li>
              ))}
            </ul>
          ) : (
            <p className={own.quiet}>{t("apps.test.noSamples")}</p>
          )}
        </div>
      ) : null}
      {test?.status === "failed" ? (
        <div role="alert">
          <p className={own.testLine}>{test.title}</p>
          {test.hint ? <p className={own.quiet}>{test.hint}</p> : null}
          {test.details ? (
            <details className={own.testDetails}>
              <summary>{t("apps.sheet.details")}</summary>
              <p>{test.details}</p>
            </details>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** Whether this toolkit is a tile that shows a Connect button (kept for callers that build tiles). */
export function toolkitTileState(
  toolkit: ComposioToolkit,
  apps: readonly ComposioApp[],
  linking: string | null
): TileState {
  return composioTileState(
    apps.find((a) => a.slug === toolkit.slug),
    linking === toolkit.slug
  );
}
