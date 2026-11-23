import { Download } from "lucide-react";
import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { formatElapsed } from "../lib/activityPanel";
import {
  buildRunReport,
  renderHtml,
  renderMarkdown,
  saveTextFile,
  type ReportText
} from "../lib/runReport";
import { renderRunReportJson } from "../lib/runReportJson";
import type { RunState } from "../state/timeline";
import { DialogBody, DialogFooter, DialogHeader } from "./DialogParts";
import { Modal } from "./Modal";
import { useToast } from "./Toast";
import dialog from "./Dialog.module.css";
import styles from "./RunTimelineModal.module.css";

/** What a bot did in one finished run, step by step, with redacted report exports. */
export function RunTimelineModal({
  run,
  botName,
  onClose
}: {
  run: RunState | null;
  botName: string;
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const report = useMemo(
    () => (run ? buildRunReport(run, botName, (k, v) => t(k, v) as string, formatElapsed) : null),
    [run, botName, t]
  );
  const text = useMemo((): ReportText => {
    const time = (ms: number) =>
      new Date(ms).toLocaleTimeString(i18n.language, { hour: "2-digit", minute: "2-digit", second: "2-digit" });
    const facts = report
      ? [
          { label: t("runReport.outcome"), value: t(`runReport.outcomes.${report.outcome}`) },
          {
            label: t("runReport.started"),
            value: new Date(report.startedMs).toLocaleString(i18n.language)
          },
          ...(report.finishedMs !== null
            ? [{ label: t("runReport.took"), value: formatElapsed(report.finishedMs - report.startedMs) }]
            : [])
        ]
      : [];
    return {
      title: t("runReport.title"),
      facts,
      noSteps: t("runReport.noSteps"),
      footer: t("runReport.footer"),
      language: i18n.language,
      time
    };
  }, [report, t, i18n.language]);

  const save = (kind: "html" | "md" | "json") => {
    if (!report || !run) return;
    if (kind === "json") {
      saveTextFile("run-report.json", "application/json", renderRunReportJson(run));
    } else {
      const body = kind === "html" ? renderHtml(report, text) : renderMarkdown(report, text);
      saveTextFile(`run-${run.id.slice(0, 8)}.${kind}`, kind === "html" ? "text/html" : "text/markdown", body);
    }
    toast.show(t("runReport.exported"), "success");
  };

  return (
    <Modal open={run !== null} onClose={onClose} size="md" label={t("runReport.title")}>
      {report ? (
        <div className={dialog.root}>
          <DialogHeader title={t("runReport.title")} subtitle={report.botName} onClose={onClose} />
          <DialogBody>
            <p className={styles.fact}>
              {text.facts.map((f) => `${f.label}: ${f.value}`).join(" · ")}
            </p>
            {report.error ? <p className={styles.error}>{report.error}</p> : null}
            {report.steps.length === 0 ? (
              <p className={styles.fact}>{text.noSteps}</p>
            ) : (
              <ol className={styles.list}>
                {report.steps.map((s, i) => (
                  <li key={i} className={styles.step} data-tone={s.tone}>
                    <span className={styles.time}>{text.time(s.atMs)}</span>
                    <span className={styles.title}>{s.title}</span>
                    {s.details.map((d, j) => (
                      <span key={j} className={styles.detail}>{d}</span>
                    ))}
                  </li>
                ))}
              </ol>
            )}
            <p className={styles.fact}>{t("runReport.privacy")}</p>
          </DialogBody>
          <DialogFooter>
            <Button variant="ghost" onClick={() => save("md")}>
              <Download aria-hidden="true" />{t("runReport.exportMarkdown")}
            </Button>
            <Button variant="ghost" onClick={() => save("json")}>
              <Download aria-hidden="true" />{t("runReport.exportJson")}
            </Button>
            <Button onClick={() => save("html")}>
              <Download aria-hidden="true" />{t("runReport.exportHtml")}
            </Button>
          </DialogFooter>
        </div>
      ) : null}
    </Modal>
  );
}
