import { Check } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ComposioAction, ComposioApp } from "../api/types";
import { AppLogo } from "../components/AppLogo";
import { BotAccessPicker } from "../components/BotAccessPicker";
import type { AppTestState } from "../lib/appTest";
import type { FlowState } from "../lib/connectFlow";
import { humanLine } from "../lib/humanError";
import { botScope, botSelection } from "../lib/mcpApps";
import { useDaemon } from "../state/DaemonContext";
import own from "./AppsGrid.module.css";
import { ComposioTestResult } from "./AppsGrid";
import styles from "./AppsModal.module.css";
import { ConnectSheet } from "./ConnectSheet";

const ACTIONS_PER_PAGE = 20;

function ToolkitActions({ slug }: { slug: string }) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [state, setState] = useState<{ actions: ComposioAction[] } | { error: string } | null>(null);
  const [retry, setRetry] = useState(0);
  const [limit, setLimit] = useState(ACTIONS_PER_PAGE);
  useEffect(() => {
    if (!client) return;
    let disposed = false;
    void client.composioActions(slug).then(
      (actions) => { if (!disposed) setState({ actions }); },
      (error: unknown) => { if (!disposed) setState({ error: humanLine(error) }); }
    );
    return () => { disposed = true; };
  }, [client, slug, retry]);

  if (!state) return <p className={own.quiet} role="status">{t("apps.composioHub.actionsLoading")}</p>;
  if ("error" in state) {
    return (
      <div className={styles.actions}>
        <p className={own.quiet} role="alert">{state.error}</p>
        <Button size="sm" variant="secondary" onClick={() => { setState(null); setRetry((n) => n + 1); }}>
          {t("apps.composio.retry")}
        </Button>
      </div>
    );
  }
  if (state.actions.length === 0) return <p className={own.quiet}>{t("apps.composioHub.actionsEmpty")}</p>;
  return (
    <>
      <ul className={own.actionList}>
        {state.actions.slice(0, limit).map((action) => {
          const label = action.name.replaceAll("_", " ");
          return (
            <li className={own.actionItem} key={action.name}>
              <strong>{label.charAt(0).toUpperCase() + label.slice(1)}</strong>
              <span
                className={`${own.actionRisk} ${own[`actionRisk_${action.class}`] ?? ""}`}
                data-risk={action.class}
                title={t(`apps.composioHub.actionClassHint.${action.class}`)}
              >
                {t(`apps.composioHub.actionClass.${action.class}`)}
              </span>
              {action.description ? <p>{action.description}</p> : null}
            </li>
          );
        })}
      </ul>
      {limit < state.actions.length ? (
        <div className={styles.actions}>
          <Button size="sm" variant="secondary" onClick={() => setLimit((n) => n + ACTIONS_PER_PAGE)}>
            {t("apps.composio.loadMore")}
          </Button>
        </div>
      ) : null}
    </>
  );
}

export function ComposioAppPage({
  slug, name, description, app, flow, test, pollError, linked, busy, onConnect,
  onAccount, onCancel, onCloseFlow, onTest, onSaveBots, onEnabled, onDisconnect
}: {
  slug: string;
  name: string;
  description?: string;
  app?: ComposioApp;
  flow: FlowState | null;
  test?: AppTestState;
  pollError?: string;
  linked: boolean;
  busy: boolean;
  onConnect: () => void;
  onAccount: () => void;
  onCancel: () => void;
  onCloseFlow: () => void;
  onTest: () => void;
  onSaveBots: (bots: string[] | null) => void;
  onEnabled: (enabled: boolean) => void;
  onDisconnect: () => void;
}) {
  const { t } = useTranslation();
  const { bots } = useDaemon();
  const heading = useRef<HTMLHeadingElement>(null);
  const [confirm, setConfirm] = useState(false);
  const [selection, setSelection] = useState<ReturnType<typeof botScope> | null>(null);
  const scope = selection ?? botSelection(botScope({ bots: app?.bots ?? null }), bots);
  const active = linked && app?.state === "active";
  const pending = linked && app?.state === "pending";
  const account = test?.status === "ok" ? test.account ?? app?.account : app?.account;

  useEffect(() => { heading.current?.focus(); }, []);

  return (
    <article className={own.appPage} data-testid={`composio-app-${slug}`} data-state={app?.state ?? "not_connected"}>
      <div className={own.appHeader}>
        <AppLogo name={name} slug={slug} size={48} />
        <div className={own.accountCopy}>
          <h2 ref={heading} tabIndex={-1} className={own.appTitle}>{name}</h2>
          {description ? <p className={own.appSummary}>{description}</p> : null}
        </div>
      </div>

      <section className={own.accountCard} aria-label={t("apps.composioHub.connection")}>
        <div className={own.accountRow}>
          <div className={own.accountCopy} role="status">
            {active ? (
              <span className={`${styles.state} ${styles.tone_ok}`}>
                <Check aria-hidden="true" />
                {account ? t("apps.connectedAs", { account }) : t("apps.composio.state.active")}
              </span>
            ) : (
              <p className={own.quiet}>
                {pending ? t("apps.composio.waiting")
                  : linked && app?.state === "expired" ? t("apps.composio.expiredHint")
                    : linked && app?.state === "failed" ? t("apps.composio.failedHint")
                      : t("apps.composio.state.not_connected")}
              </p>
            )}
          </div>
          <div className={styles.actions}>
            {!linked ? (
              <Button size="sm" variant="primary" onClick={onAccount}>{t("apps.composioHub.linkAccount")}</Button>
            ) : active || pending || flow ? null : (
              <Button size="sm" variant="primary" disabled={busy} onClick={onConnect} data-testid={`composio-connect-${slug}`}>
                {t(app?.state === "expired" ? "apps.reconnect" : app?.state === "failed" ? "apps.composio.retry" : "apps.connect")}
              </Button>
            )}
            {pending && !flow ? <Button size="sm" variant="ghost" disabled={busy} onClick={onCancel}>{t("common.cancel")}</Button> : null}
            {linked && app && !pending && !confirm ? (
              <Button size="sm" variant="ghost" disabled={busy} onClick={() => setConfirm(true)}
                data-testid={`composio-disconnect-${slug}`}>{t("apps.disconnect")}</Button>
            ) : null}
          </div>
        </div>
        {flow ? <ConnectSheet flow={flow} onCancel={onCancel} onRetry={onConnect} onClose={onCloseFlow} /> : null}
        {pending && pollError ? <p className={own.quiet} role="status">{pollError}</p> : null}
        {confirm ? (
          <div className={styles.actions}>
            <Button size="sm" variant="danger" disabled={busy} data-testid={`composio-disconnect-confirm-${slug}`}
              onClick={() => { setConfirm(false); onDisconnect(); }}>{t("apps.composioHub.disconnectConfirm")}</Button>
            <Button size="sm" variant="ghost" onClick={() => setConfirm(false)}>{t("common.cancel")}</Button>
          </div>
        ) : null}
        {active ? <ComposioTestResult slug={slug} test={test} busy={busy} onTest={onTest} /> : null}
      </section>

      {active ? (
        <section className={own.appSection} aria-labelledby="composio-access-heading">
          <h3 className={own.sectionTitle} id="composio-access-heading">{t("apps.access.label")}</h3>
          <BotAccessPicker scope={scope} bots={bots} onChange={setSelection} />
          <div className={styles.actions}>
            <Button size="sm" variant="primary" disabled={busy || selection === null}
              data-testid={`composio-access-save-${slug}`}
              onClick={() => onSaveBots(scope.mode === "all" ? null : scope.ids)}>{t("apps.access.save")}</Button>
            <Button size="sm" variant="ghost" disabled={busy}
              onClick={() => onEnabled(!app.enabled)}>
              {t(app.enabled ? "apps.composioHub.pauseAccess" : "apps.composioHub.resumeAccess")}
            </Button>
          </div>
        </section>
      ) : null}

      <section className={own.appSection} aria-labelledby="composio-actions-heading">
        <h3 className={own.sectionTitle} id="composio-actions-heading">{t("apps.composioHub.actionsTitle")}</h3>
        {linked ? <ToolkitActions slug={slug} /> : <p className={own.quiet}>{t("apps.composioHub.actionsNeedAccount")}</p>}
      </section>
    </article>
  );
}
