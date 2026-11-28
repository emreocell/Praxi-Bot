import { Check, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { EmailAccount, EmailChannel, EmailChannelSettings, EmailPendingReply } from "../../api/types";
import { CardSaveBar, FormField, formStyles } from "../../components/SettingsForm";
import { MoreInfo } from "../../components/ChoiceList";
import { SettingsCard, SwitchField, SwitchGrid } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import { humanLine } from "../../lib/humanError";
import { openSettingsTarget } from "../../lib/settingsLink";
import { useDaemon } from "../../state/DaemonContext";
import mailStyles from "./EmailCard.module.css";
import styles from "./EmailChannelCard.module.css";

const K = "settingsModal.emailChannel";

function settingsEqual(a: EmailChannelSettings, b: EmailChannelSettings): boolean {
  return (
    a.account === b.account &&
    a.folder === b.folder &&
    a.bot === b.bot &&
    a.mode === b.mode &&
    a.replies_enabled === b.replies_enabled &&
    a.enabled === b.enabled
  );
}

/**
 * Settings card "E-mail to a bot": new mail in one folder reaches one bot as
 * untrusted text (a short summary unless the owner asks for the full text).
 */
export function EmailChannelCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, bots } = useDaemon();
  const [accounts, setAccounts] = useState<EmailAccount[]>([]);
  const [channel, setChannel] = useState<EmailChannel | null>(null);
  const [form, setForm] = useState<EmailChannelSettings | null>(null);
  const [saved, setSaved] = useState<EmailChannelSettings | null>(null);
  const [replies, setReplies] = useState<EmailPendingReply[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      const [accts, ch, pending] = await Promise.all([
        client.listEmailAccounts(),
        client.getEmailChannel(),
        client.listEmailReplies()
      ]);
      setAccounts(accts.filter((a) => a.provider === "imap_smtp"));
      setChannel(ch);
      const settings = ch.settings;
      setForm((old) => (old && !settingsEqual(old, settings) ? old : settings));
      setSaved(settings);
      setReplies(pending);
      setError(null);
    } catch (e) {
      setError(humanLine(e));
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const dirty = form && saved ? !settingsEqual(form, saved) : false;

  const save = async () => {
    if (!client || !form) {
      return;
    }
    setBusy(true);
    try {
      const next = await client.saveEmailChannel(form);
      setChannel(next);
      setForm(next.settings);
      setSaved(next.settings);
      toast.show(t(`${K}.saved`), "success");
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
    }
  };

  const decide = async (id: string, approve: boolean) => {
    if (!client) {
      return;
    }
    setBusy(true);
    try {
      if (approve) {
        await client.approveEmailReply(id);
        toast.show(t(`${K}.sent`), "success");
      } else {
        await client.denyEmailReply(id);
      }
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
      await reload();
    }
  };

  const selected = form ? accounts.find((a) => a.name === form.account) : undefined;
  const set = (patch: Partial<EmailChannelSettings>) => setForm((f) => (f ? { ...f, ...patch } : f));

  const status = useMemo(() => {
    if (!form) {
      return null;
    }
    if (!form.account) {
      return {
        tone: "warn" as const,
        text: t(`${K}.status.needMailbox`),
        action: (
          <Button size="sm" variant="ghost" onClick={() => openSettingsTarget({ page: "connections", card: "email", field: "" })}>
            {t(`${K}.status.openMailbox`)}
          </Button>
        )
      };
    }
    const mailboxSenderFix = (
      <Button size="sm" variant="ghost" onClick={() => openSettingsTarget({ page: "connections", card: "email", field: "" })}>
        {t(`${K}.status.openMailbox`)}
      </Button>
    );
    if (form.enabled && channel?.needs_senders) {
      return {
        tone: "bad" as const,
        text: t(`${K}.status.noSendersTitle`),
        detail: t(`${K}.needsSenders`),
        action: mailboxSenderFix
      };
    }
    if (selected && selected.allowed_senders.length === 0) {
      return {
        tone: "bad" as const,
        text: t(`${K}.status.noSendersTitle`),
        detail: t(`${K}.status.noSendersDetail`),
        action: mailboxSenderFix
      };
    }
    if (!form.enabled) {
      return { tone: "muted" as const, text: t(`${K}.status.off`) };
    }
    if (dirty) {
      return { tone: "warn" as const, text: t(`${K}.status.unsaved`) };
    }
    if (channel?.running) {
      return { tone: "ok" as const, text: t(`${K}.running`) };
    }
    return {
      tone: "warn" as const,
      text: t(`${K}.stopped`),
      detail: t(`${K}.status.stoppedHint`)
    };
  }, [channel?.needs_senders, channel?.running, dirty, form, selected, t]);

  return (
    <SettingsCard
      title={t(`${K}.title`)}
      description={t(`${K}.description`)}
      testId="email-channel-card"
    >
      <div className={styles.root}>
        <MoreInfo testId="email-channel-more">
          <p>{t(`${K}.note`)}</p>
        </MoreInfo>
        {error ? (
          <p className={formStyles.error} role="alert">
            {error}
          </p>
        ) : null}
        {form ? (
          <>
            <SwitchGrid>
              <SwitchField
                checked={form.enabled}
                onChange={(v) => set({ enabled: v })}
                label={t(`${K}.enable`)}
                hint={t(`${K}.enableHint`)}
                testId="email-channel-enabled"
              />
            </SwitchGrid>
            {form.enabled ? (
              <div className={styles.section}>
                {accounts.length === 0 ? (
                  <p className={formStyles.hint}>{t(`${K}.noAccount`)}</p>
                ) : (
                  <FormField
                    id="email-channel-account"
                    label={t(`${K}.account`)}
                    hint={t(`${K}.accountHint`)}
                    width="md"
                  >
                    <select
                      className={formStyles.select}
                      value={form.account}
                      data-testid="email-channel-account"
                      onChange={(e) => set({ account: e.target.value })}
                    >
                      <option value="">{t(`${K}.choose`)}</option>
                      {accounts.map((a) => (
                        <option key={a.name} value={a.name}>
                          {a.name} ({a.address})
                        </option>
                      ))}
                    </select>
                  </FormField>
                )}
                <div className={styles.pair}>
                  <FormField id="email-channel-folder" label={t(`${K}.folder`)} hint={t(`${K}.folderHint`)} width="sm">
                    <input
                      className={formStyles.input}
                      value={form.folder}
                      data-testid="email-channel-folder"
                      maxLength={200}
                      onChange={(e) => set({ folder: e.target.value })}
                    />
                  </FormField>
                  <FormField id="email-channel-bot" label={t(`${K}.bot`)} width="md">
                    <select
                      className={formStyles.select}
                      value={form.bot ?? ""}
                      data-testid="email-channel-bot"
                      onChange={(e) => set({ bot: e.target.value || null })}
                    >
                      <option value="">{t(`${K}.choose`)}</option>
                      {bots.map((b) => (
                        <option key={b.id} value={b.id}>
                          {b.name}
                        </option>
                      ))}
                    </select>
                  </FormField>
                </div>
                <SwitchGrid>
                  <SwitchField
                    checked={form.mode === "full"}
                    onChange={(v) => set({ mode: v ? "full" : "summary" })}
                    label={t(`${K}.full`)}
                    hint={t(`${K}.fullHint`)}
                    testId="email-channel-full"
                  />
                  <SwitchField
                    checked={form.replies_enabled}
                    onChange={(v) => set({ replies_enabled: v })}
                    label={t(`${K}.replies`)}
                    hint={t(`${K}.repliesHint`)}
                    testId="email-channel-replies"
                  />
                </SwitchGrid>
              </div>
            ) : null}
            <CardSaveBar
              saveLabel={t(`${K}.save`)}
              onSave={() => void save()}
              saving={busy}
              disabled={!dirty || !form.account}
              statusText={status?.text}
              statusTone={status?.tone}
              statusDetail={status?.detail}
              action={status?.action}
              testId="email-channel-save-bar"
            />
          </>
        ) : null}
        {channel && channel.ignored_mails > 0 ? (
          <p className={formStyles.hint} data-testid="email-channel-ignored">
            {t(`${K}.ignored`, { n: channel.ignored_mails })}
          </p>
        ) : null}
        {channel && channel.skipped_mails > 0 ? (
          <p className={formStyles.error} role="status" data-testid="email-channel-skipped">
            {t(`${K}.skipped`, { n: channel.skipped_mails })}
          </p>
        ) : null}
        {replies.length > 0 ? (
          <div className={mailStyles.info}>
            <h3 className={mailStyles.heading}>{t(`${K}.waiting`)}</h3>
            {replies.map((r) => (
              <div key={r.id} className={styles.reply} data-testid={`email-reply-${r.id}`}>
                <p className={styles.recipients}>{t(`${K}.to`, { to: r.to.join(", ") })}</p>
                {r.from ? (
                  <p className={formStyles.hint} data-testid={`email-reply-from-${r.id}`}>
                    {t(`${K}.fromLine`, { from: r.from })}
                  </p>
                ) : null}
                <p className={formStyles.hint}>{t(`${K}.subject`, { subject: r.subject })}</p>
                <pre className={styles.body}>{r.body}</pre>
                <div className={mailStyles.actions}>
                  <Button size="sm" disabled={busy} data-testid={`email-reply-approve-${r.id}`} onClick={() => void decide(r.id, true)}>
                    <Check />
                    {t(`${K}.approve`)}
                  </Button>
                  <Button size="sm" variant="ghost" disabled={busy} data-testid={`email-reply-deny-${r.id}`} onClick={() => void decide(r.id, false)}>
                    <X />
                    {t(`${K}.deny`)}
                  </Button>
                </div>
              </div>
            ))}
          </div>
        ) : null}
      </div>
    </SettingsCard>
  );
}