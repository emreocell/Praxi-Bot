import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { type FlowState, canRetry } from "../lib/connectFlow";
import own from "./ConnectSheet.module.css";

/** Progress and errors for the toolkit's hosted consent. */
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
  const text =
    flow.step === "app"
      ? t("apps.sheet.app", { name: flow.name })
      : flow.step === "opened"
        ? t("apps.sheet.opened", { name: flow.name })
        : flow.step === "failed"
          ? t(`apps.sheet.failed.${flow.reason ?? "app_link"}`, { name: flow.name })
          : t("apps.sheet.cancelled");
  return (
    <div className={own.sheet} data-testid={`connect-sheet-${flow.slug}`} data-step={flow.step}>
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
        {flow.step === "app" || flow.step === "opened" ? (
          <Button size="sm" variant="ghost" data-testid="connect-sheet-cancel" onClick={onCancel}>
            {t("common.cancel")}
          </Button>
        ) : (
          <>
            {flow.step !== "cancelled" && (flow.step !== "failed" || canRetry(flow.reason)) ? (
              <Button size="sm" variant="secondary" data-testid="connect-sheet-retry" onClick={onRetry}>
                {t("apps.composio.retry")}
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
