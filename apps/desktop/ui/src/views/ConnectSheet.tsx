import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { AppLogo } from "../components/AppLogo";
import { type FlowState, canRetry, flowActive, progressDot } from "../lib/connectFlow";
import own from "./ConnectSheet.module.css";

/**
 * The small sheet anchored to an app tile while "Connect" runs: the app's logo, its name and
 * two progress dots (Composio sign-in the first time, then the app's own consent page). One
 * flow, one press: it carries on by itself when the sign-in finishes.
 */
export function ConnectSheet({
  flow,
  onCancel,
  onRetry,
  onClose
}: {
  flow: FlowState;
  onCancel: () => void;
  onRetry: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const active = flowActive(flow);
  const dots = flow.firstTime ? [1, 2] : [1];
  const current = flow.firstTime ? progressDot(flow) : 1;
  const text =
    flow.step === "login"
      ? t("apps.sheet.login", { name: flow.name })
      : flow.step === "app"
        ? t("apps.sheet.app", { name: flow.name })
        : flow.step === "opened"
          ? t("apps.sheet.opened", { name: flow.name })
          : flow.step === "failed"
            ? t(`apps.sheet.failed.${flow.reason ?? "app_link"}`, { name: flow.name })
            : flow.step === "timed_out"
              ? t("apps.sheet.timedOut")
              : t("apps.sheet.cancelled");
  return (
    <div className={own.sheet} role="dialog" aria-label={t("apps.sheet.title", { name: flow.name })} data-testid={`connect-sheet-${flow.slug}`} data-step={flow.step}>
      <div className={own.head}>
        <AppLogo name={flow.name} slug={flow.slug} size={32} />
        <b className={own.title}>{t("apps.sheet.title", { name: flow.name })}</b>
        <span className={own.dots} role="img" aria-label={t("apps.sheet.progress", { current, total: dots.length })}>
          {dots.map((n) => (
            <i key={n} className={`${own.dot} ${n <= current && flow.step !== "failed" ? own.dotOn : ""}`} />
          ))}
        </span>
      </div>
      <p className={own.text} role="status">{text}</p>
      {flow.step === "failed" && flow.failure ? (
        <details className={own.details} data-testid="connect-sheet-details">
          <summary>{t("apps.sheet.details")}</summary>
          {[flow.failure.title, flow.failure.hint, flow.failure.details].filter(Boolean).map((line) => (
            <p key={line}>{line}</p>
          ))}
        </details>
      ) : null}
      <div className={own.actions}>
        {active || flow.step === "opened" ? (
          <Button size="sm" variant="ghost" data-testid="connect-sheet-cancel" onClick={onCancel}>
            {t("common.cancel")}
          </Button>
        ) : (
          <>
            {flow.step !== "cancelled" && (flow.step !== "failed" || canRetry(flow.reason)) ? (
              <Button size="sm" variant="secondary" data-testid="connect-sheet-retry" onClick={onRetry}>
                {flow.step === "failed" && flow.reason === "login_expired" ? t("apps.composio.relogin") : t("apps.composio.retry")}
              </Button>
            ) : null}
            <Button size="sm" variant="ghost" onClick={onClose}>
              {t("common.close")}
            </Button>
          </>
        )}
      </div>
    </div>
  );
}
