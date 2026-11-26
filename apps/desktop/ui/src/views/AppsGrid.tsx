import { Check, ChevronRight } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ComposioApp } from "../api/types";
import { AppLogo } from "../components/AppLogo";
import { Skeleton, SkeletonLines } from "../components/Skeleton";
import { type AppTestState } from "../lib/appTest";
import { GROUPS, tileAction, type TileState } from "../lib/appTiles";
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
      {["all", ...GROUPS, "other"].map((g) => (
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
  onConnect: () => void;
  /** Stops a pending sign-in (shown next to the waiting state). */
  onCancel?: () => void;
}) {
  const { t } = useTranslation();
  const action = tileAction(state);
  return (
    <li className={`${styles.cell}`} data-testid={testId} data-state={state}>
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

/** One entry point for a toolkit, whether it has been connected or not. */
export function ComposioCard({ slug, name, description, app, onOpen }: {
  slug: string;
  name: string;
  description?: string;
  app?: ComposioApp;
  onOpen: () => void;
}) {
  const { t } = useTranslation();
  const state = app?.state ?? "not_connected";
  return (
    <li className={styles.cell} data-testid={`composio-tile-${slug}`} data-state={state}>
      <button className={`${styles.cellRow} ${own.cardButton}`} type="button" onClick={onOpen}>
        <AppLogo name={name} slug={slug} />
        <span className={styles.cellText}>
          <span className={styles.cellName}><span className={styles.cellTitle}>{name}</span></span>
          {description ? <span className={styles.cellSub}>{description}</span> : null}
        </span>
        {state !== "not_connected" ? (
          <span className={`${styles.state} ${styles[`tone_${tone(state)}`]}`}>
            {state === "active" ? <Check aria-hidden="true" /> : <span className={styles.dot} aria-hidden="true" />}
            {t(`apps.composio.state.${state}`)}
          </span>
        ) : null}
        <ChevronRight className={own.cardArrow} aria-hidden="true" />
      </button>
    </li>
  );
}

/** The "Test connection" row of a connected app: the button, and what the last test found. */
export function ComposioTestResult({
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
            {test.authorizationOnly
              ? t("apps.composioHub.authorizationVerified")
              : test.account ? t("apps.test.okAs", { account: test.account }) : t("apps.test.ok")}
          </p>
          {test.items.length > 0 ? (
            <ul className={own.testItems} aria-label={t("apps.test.samples")}>
              {test.items.map((item, i) => (
                <li key={`${i}-${item}`}>{item}</li>
              ))}
            </ul>
          ) : null}
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
