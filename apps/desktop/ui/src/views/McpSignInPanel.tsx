import { type ReactNode, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import formStyles from "../components/Form.module.css";
import { openSignInLink } from "../lib/externalLink";
import { type SignInState, signInRetryable } from "../lib/mcpSignIn";
import own from "./McpSignInPanel.module.css";

/**
 * Where a sign-in stands, in words: the page is open in the browser, the answer comes back by
 * itself (or, when this window cannot catch it, the one address to paste), what failed and what to
 * do about it. One at a time, at the top of the Apps dialog.
 */
export function McpSignInPanel({
  state,
  title,
  onCancel,
  onPaste,
  onRetry,
  onClose,
  children
}: {
  state: SignInState;
  title: string;
  onCancel: () => void;
  onPaste: (url: string) => void;
  onRetry: () => void;
  onClose: () => void;
  children?: ReactNode;
}) {
  const { t } = useTranslation();
  const [pasted, setPasted] = useState("");
  const [copied, setCopied] = useState(false);
  const running = state.step === "starting" || state.step === "browser" || state.step === "finishing";
  const text =
    state.step === "starting"
      ? t("apps.signIn.starting")
      : state.step === "finishing"
        ? t("apps.signIn.finishing")
        : state.step === "browser"
          ? state.mode === "paste"
            ? t("apps.signIn.paste.body")
            : state.mode === "relay"
              ? t("apps.signIn.waitingServer")
              : t("apps.signIn.waiting")
          : state.step === "failed"
            ? t(`apps.signIn.failed.${state.reason}`)
            : state.step === "done"
              ? t("apps.signIn.done")
              : null;
  return (
    <section
      className={own.panel}
      role="group"
      aria-label={t("apps.signIn.title", { name: title })}
      data-testid="mcp-sign-in-panel"
      data-step={state.step}
      data-mode={state.step === "browser" ? state.mode : undefined}
    >
      <div className={own.head}>
        <b className={own.title}>{t("apps.signIn.title", { name: title })}</b>
      </div>
      {text ? (
        <p className={`${own.text} ${state.step === "failed" ? own.bad : ""}`} role={state.step === "failed" ? "alert" : "status"}>
          {text}
        </p>
      ) : null}
      {state.step === "browser" && !state.opened ? (
        <p className={`${own.text} ${own.bad}`} role="alert">
          {t("apps.signIn.notOpened")}
        </p>
      ) : null}
      {state.step === "browser" ? (
        <>
          <div className={own.row}>
            <Button size="sm" variant="secondary" data-testid="mcp-sign-in-open" onClick={() => void openSignInLink(state.url)}>
              {state.opened ? t("apps.signIn.openAgain") : t("apps.signIn.open")}
            </Button>
            <Button
              size="sm"
              variant="ghost"
              data-testid="mcp-sign-in-copy"
              onClick={() => {
                void navigator.clipboard?.writeText(state.url).then(
                  () => setCopied(true),
                  () => setCopied(false)
                );
              }}
            >
              {copied ? t("apps.signIn.linkCopied") : t("apps.signIn.copyLink")}
            </Button>
          </div>
          {state.mode === "paste" ? (
            <form
              className={own.row}
              onSubmit={(e) => {
                e.preventDefault();
                if (pasted.trim()) {
                  onPaste(pasted.trim());
                  setPasted("");
                }
              }}
            >
              <input
                className={`${formStyles.input} ${own.field}`}
                value={pasted}
                onChange={(e) => setPasted(e.target.value)}
                placeholder="http://127.0.0.1:…/callback?…"
                aria-label={t("apps.signIn.paste.label")}
                data-testid="mcp-sign-in-paste"
              />
              <Button size="sm" variant="primary" type="submit" disabled={!pasted.trim()}>
                {t("apps.signIn.paste.submit")}
              </Button>
            </form>
          ) : null}
        </>
      ) : null}
      {children}
      {state.step === "failed" && state.detail ? (
        <details className={own.details}>
          <summary>{t("apps.signIn.details")}</summary>
          <pre>{state.detail}</pre>
        </details>
      ) : null}
      <div className={own.row}>
        {running ? (
          <Button size="sm" variant="ghost" data-testid="mcp-sign-in-cancel" onClick={onCancel}>
            {t("common.cancel")}
          </Button>
        ) : (
          <>
            {state.step === "failed" && signInRetryable(state.reason) ? (
              <Button size="sm" variant="secondary" data-testid="mcp-sign-in-retry" onClick={onRetry}>
                {t("apps.signIn.retry")}
              </Button>
            ) : null}
            <Button size="sm" variant="ghost" onClick={onClose}>
              {t("common.close")}
            </Button>
          </>
        )}
      </div>
    </section>
  );
}
