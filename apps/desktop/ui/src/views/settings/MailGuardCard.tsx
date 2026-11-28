import { Download, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { EmailGuardSettings, EmailOptOut, EmailSendLogEntry } from "../../api/types";
import { CardSaveBar, FormField, formStyles } from "../../components/SettingsForm";
import { SettingsCard, SwitchField } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import { type HumanError, humanError, humanLine } from "../../lib/humanError";
import { formatDateTime } from "../../lib/messageTime";
import { saveTextFile } from "../../lib/runReport";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./MailGuardCard.module.css";

const LOG_ROWS = 100;
const K = "settingsModal.mailGuard";

/** "Safe sending": the limits, the opt-out list and the log of every mail the bots sent. */
export function MailGuardCard() {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const [settings, setSettings] = useState<EmailGuardSettings | null>(null);
  const [saved, setSaved] = useState<EmailGuardSettings | null>(null);
  const [optOuts, setOptOuts] = useState<EmailOptOut[]>([]);
  const [log, setLog] = useState<EmailSendLogEntry[]>([]);
  const [entry, setEntry] = useState("");
  const [domains, setDomains] = useState("");
  const [busy, setBusy] = useState(false);

  const [problems, setProblems] = useState<Record<"settings" | "optOuts" | "log", HumanError | null>>({
    settings: null,
    optOuts: null,
    log: null
  });

  const load = useCallback(async () => {
    if (!client) return;
    const [s, o, l] = await Promise.allSettled([
      client.getMailGuardSettings(),
      client.listMailOptOuts(),
      client.listMailSendLog(LOG_ROWS)
    ]);
    if (s.status === "fulfilled") {
      setSettings(s.value);
      setSaved(s.value);
      setDomains((s.value.verification_link_domains ?? []).join("\n"));
    }
    if (o.status === "fulfilled") setOptOuts(o.value);
    if (l.status === "fulfilled") setLog(l.value);
    setProblems({
      settings: s.status === "rejected" ? humanError(s.reason) : null,
      optOuts: o.status === "rejected" ? humanError(o.reason) : null,
      log: l.status === "rejected" ? humanError(l.reason) : null
    });
  }, [client]);

  useEffect(() => {
    void load();
  }, [load]);

  const act = async (work: () => Promise<void>) => {
    setBusy(true);
    try {
      await work();
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
    }
  };

  const number = (v: string) => Math.max(0, Math.min(100000, Number.parseInt(v, 10) || 0));

  const note = (problem: HumanError | null, testId: string) =>
    problem ? (
      <p className={formStyles.hint} role="status" data-testid={testId}>
        {problem.title}
        {problem.hint ? ` ${problem.hint}` : ""}{" "}
        <Button size="sm" variant="ghost" onClick={() => void load()}>
          {t(`${K}.retry`)}
        </Button>
      </p>
    ) : null;

  const dirty =
    settings && saved
      ? settings.daily_limit !== saved.daily_limit ||
        settings.same_recipient_days !== saved.same_recipient_days ||
        settings.opt_out_line !== saved.opt_out_line ||
        settings.footer !== saved.footer ||
        domains !== (saved.verification_link_domains ?? []).join("\n")
      : false;

  if (!settings) {
    return (
      <SettingsCard title={t(`${K}.title`)} description={t(`${K}.description`)} testId="mail-guard">
        {problems.settings ? note(problems.settings, "mail-guard-load-error") : t(`${K}.loading`)}
      </SettingsCard>
    );
  }

  const persist = () =>
    act(async () => {
      const next = await client!.putMailGuardSettings({
        ...settings,
        verification_link_domains: domains.split(/[\s,;]+/).filter(Boolean)
      });
      setSettings(next);
      setSaved(next);
      setDomains((next.verification_link_domains ?? []).join("\n"));
      toast.show(t(`${K}.saved`), "success");
    });

  return (
    <SettingsCard title={t(`${K}.title`)} description={t(`${K}.description`)} testId="mail-guard">
      <div className={styles.root} data-testid="mail-guard-body">
        <p className={styles.intro}>{t(`${K}.intro`)}</p>

        <section className={styles.limits} aria-labelledby="mail-guard-limits">
          <h3 id="mail-guard-limits" className={styles.sectionTitle}>
            {t(`${K}.limitsTitle`)}
          </h3>
          <p className={formStyles.hint}>{t(`${K}.softHint`)}</p>
          <div className={styles.limitRow}>
            <FormField
              id="mg-daily"
              label={t(`${K}.daily`)}
              helpTitle={t(`${K}.daily`)}
              helpBody={t(`${K}.dailyHelp`)}
              width="sm"
            >
              <input
                id="mg-daily"
                className={formStyles.input}
                type="number"
                min={0}
                value={settings.daily_limit}
                onChange={(e) => setSettings({ ...settings, daily_limit: number(e.target.value) })}
              />
            </FormField>
            <FormField
              id="mg-days"
              label={t(`${K}.days`)}
              helpTitle={t(`${K}.days`)}
              helpBody={t(`${K}.daysHelp`)}
              width="sm"
            >
              <input
                id="mg-days"
                className={formStyles.input}
                type="number"
                min={0}
                value={settings.same_recipient_days}
                onChange={(e) => setSettings({ ...settings, same_recipient_days: number(e.target.value) })}
              />
            </FormField>
          </div>
          <SwitchField
            checked={settings.opt_out_line}
            onChange={(v) => setSettings({ ...settings, opt_out_line: v })}
            label={t(`${K}.optOutLine`)}
            hint={t(`${K}.optOutLineHint`)}
            testId="mail-guard-optout-line"
          />
          {settings.opt_out_line ? (
            <FormField
              id="mg-footer"
              label={t(`${K}.footer`)}
              hint={t(`${K}.footerShort`)}
              helpTitle={t(`${K}.footer`)}
              helpBody={t(`${K}.footerHint`)}
            >
              <textarea
                id="mg-footer"
                className={formStyles.textarea}
                rows={2}
                maxLength={1000}
                placeholder={t(`${K}.footerPlaceholder`)}
                value={settings.footer}
                onChange={(e) => setSettings({ ...settings, footer: e.target.value })}
              />
            </FormField>
          ) : null}
          <FormField
            id="mg-verify"
            label={t(`${K}.verifyDomains`)}
            hint={t(`${K}.verifyDomainsShort`)}
            helpTitle={t(`${K}.verifyDomains`)}
            helpBody={t(`${K}.verifyDomainsHint`)}
          >
            <textarea
              id="mg-verify"
              className={formStyles.textarea}
              rows={2}
              maxLength={4000}
              data-testid="mail-guard-verify-domains"
              placeholder={t(`${K}.verifyDomainsPlaceholder`)}
              value={domains}
              onChange={(e) => setDomains(e.target.value)}
            />
          </FormField>
          <CardSaveBar
            saveLabel={t(`${K}.save`)}
            onSave={() => void persist()}
            saving={busy}
            disabled={!dirty}
            statusText={dirty ? t(`${K}.status.unsaved`) : t(`${K}.status.saved`)}
            statusTone={dirty ? "warn" : "ok"}
            testId="mail-guard-save-bar"
          />
        </section>

        <section aria-labelledby="mail-guard-optouts">
          <h3 id="mail-guard-optouts" className={styles.sectionTitle}>
            {t(`${K}.optOutTitle`)}
          </h3>
          <p className={formStyles.hint}>{t(`${K}.optOutHint`)}</p>
          <div className={styles.optRow}>
            <input
              className={[formStyles.input, styles.grow].join(" ")}
              aria-label={t(`${K}.optOutAdd`)}
              placeholder={t(`${K}.optOutPlaceholder`)}
              value={entry}
              onChange={(e) => setEntry(e.target.value)}
              data-testid="mail-guard-optout-input"
            />
            <Button
              size="sm"
              variant="secondary"
              disabled={busy || entry.trim() === ""}
              data-testid="mail-guard-optout-add"
              onClick={() =>
                void act(async () => {
                  await client!.addMailOptOut(entry.trim());
                  setEntry("");
                  setOptOuts(await client!.listMailOptOuts());
                })
              }
            >
              {t(`${K}.optOutAdd`)}
            </Button>
          </div>
          {note(problems.optOuts, "mail-guard-optouts-error")}
          {problems.optOuts ? null : optOuts.length === 0 ? (
            <p className={formStyles.hint}>{t(`${K}.optOutEmpty`)}</p>
          ) : (
            <ul className={styles.list} data-testid="mail-guard-optouts">
              {optOuts.map((o) => (
                <li key={o.value}>
                  {o.value}{" "}
                  <span className={formStyles.hint}>
                    {t(o.kind === "domain" ? `${K}.kindDomain` : `${K}.kindAddress`)}
                  </span>{" "}
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    aria-label={t(`${K}.optOutRemove`, { value: o.value })}
                    title={t(`${K}.optOutRemove`, { value: o.value })}
                    onClick={() =>
                      void act(async () => {
                        await client!.removeMailOptOut(o.value);
                        setOptOuts(await client!.listMailOptOuts());
                      })
                    }
                  >
                    <Trash2 size={14} aria-hidden />
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section aria-labelledby="mail-guard-log">
          <h3 id="mail-guard-log" className={styles.sectionTitle}>
            {t(`${K}.logTitle`)}
          </h3>
          <p className={formStyles.hint}>{t(`${K}.logHint`)}</p>
          <Button
            size="sm"
            variant="secondary"
            disabled={busy}
            data-testid="mail-guard-export"
            onClick={() =>
              void act(async () => {
                saveTextFile("send-log.csv", "text/csv", await client!.exportMailSendLog());
              })
            }
          >
            <Download size={14} aria-hidden /> {t(`${K}.export`)}
          </Button>
          {note(problems.log, "mail-guard-log-error")}
          {problems.log ? null : log.length === 0 ? (
            <p className={formStyles.hint}>{t(`${K}.logEmpty`)}</p>
          ) : (
            <table className={styles.log} data-testid="mail-guard-log">
              <thead>
                <tr>
                  <th>{t(`${K}.colTime`)}</th>
                  <th>{t(`${K}.colTo`)}</th>
                  <th>{t(`${K}.colSubject`)}</th>
                  <th>{t(`${K}.colBot`)}</th>
                  <th>{t(`${K}.colNote`)}</th>
                </tr>
              </thead>
              <tbody>
                {log.map((e, i) => (
                  <tr key={`${e.at}-${e.message_id}-${i}`}>
                    <td>{formatDateTime(e.at * 1000, i18n.language)}</td>
                    <td>{e.recipients.join(", ")}</td>
                    <td>{e.subject}</td>
                    <td>{e.bot ?? ""}</td>
                    <td>{e.send_anyway.length > 0 ? t(`${K}.sentAnyway`) : ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      </div>
    </SettingsCard>
  );
}