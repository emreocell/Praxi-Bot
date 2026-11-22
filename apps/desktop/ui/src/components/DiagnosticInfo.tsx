import { ClipboardCopy, Eye } from "lucide-react";
import { useReducer } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { INITIAL_DIAGNOSTIC_INFO, diagnosticInfoReducer, textToCopy } from "../lib/diagnosticInfo";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import { useToast } from "./Toast";
import styles from "./HelpModal.module.css";

/**
 * Help > Copy diagnostic info.  The daemon builds the report with the same
 * redaction as its logs (keys, tokens, e-mail addresses and your user folder are
 * masked; no message content is included).  Nothing leaves the computer: the
 * text is shown first and only copied to the clipboard when the user presses Copy.
 */
export function DiagnosticInfo() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const [state, dispatch] = useReducer(diagnosticInfoReducer, INITIAL_DIAGNOSTIC_INFO);

  const show = async () => {
    if (!client) {
      return;
    }
    dispatch({ type: "load" });
    try {
      dispatch({ type: "loaded", text: await client.doctorReport() });
    } catch (e) {
      dispatch({ type: "failed", error: e });
    }
  };

  const copy = async () => {
    const text = textToCopy(state);
    if (text === null) {
      return;
    }
    try {
      await navigator.clipboard.writeText(text);
      toast.show(t("help.center.diag.copied"), "success");
    } catch {
      toast.show(t("help.center.diag.copyFailed"), "danger");
    }
  };

  return (
    <details className={styles.answer} data-testid="help-diagnostic-info">
      <summary className={styles.question}>{t("help.center.diag.title")}</summary>
      <p className={styles.text}>{t("help.center.diag.intro")}</p>
      <div className={styles.actions}>
        <Button variant="secondary" size="sm" disabled={!client || state.phase === "loading"} onClick={() => void show()} data-testid="help-diag-show">
          <Eye aria-hidden="true" />
          {state.phase === "ready" ? t("help.center.diag.refresh") : t("help.center.diag.show")}
        </Button>
        <Button variant="primary" size="sm" disabled={textToCopy(state) === null} onClick={() => void copy()} data-testid="help-diag-copy">
          <ClipboardCopy aria-hidden="true" />
          {t("help.center.diag.copy")}
        </Button>
      </div>
      {!client ? <p className={styles.text}>{t("help.center.diag.offline")}</p> : null}
      {state.phase === "loading" ? (
        <p className={styles.text} role="status">
          {t("help.center.diag.loading")}
        </p>
      ) : null}
      {state.phase === "error" ? (
        <p className={styles.text} role="alert">
          {t("help.center.diag.failed")} ({humanLine(state.error)})
        </p>
      ) : null}
      {state.phase === "ready" ? (
        <>
          <p className={styles.text}>{t("help.center.diag.previewNote")}</p>
          <pre className={styles.preview} tabIndex={0} aria-label={t("help.center.diag.previewLabel")} data-testid="help-diag-preview">
            {state.text}
          </pre>
        </>
      ) : null}
    </details>
  );
}
