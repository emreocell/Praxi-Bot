import { Check, Info, Mail, X } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import type { EmailAccount, EmailChannel, EmailChannelSettings, EmailPendingReply } from "../../api/types";
import formStyles from "../../components/Form.module.css";
import { useToast } from "../../components/Toast";
import { humanLine } from "../../lib/humanError";
import { useDaemon } from "../../state/DaemonContext";
import mailStyles from "./EmailCard.module.css";
import styles from "./EmailChannelCard.module.css";

/**
 * Settings card "E-mail to a bot": new mail in one folder reaches one bot as
 * untrusted text (a short summary unless the owner asks for the full text).
 * Replies are a separate opt-in and each one waits here, in plain text, with
 * every recipient and the whole message, until the owner approves it.
 * Mail text is always rendered as text, never as HTML.
 */
export function EmailChannelCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, bots } = useDaemon();
  const [accounts, setAccounts] = useState<EmailAccount[]>([]);
  const [channel, setChannel] = useState<EmailChannel | null>(null);
  const [form, setForm] = useState<EmailChannelSettings | null>(null);
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
      setForm((old) => old ?? ch.settings);
      setReplies(pending);
      setError(null);
    } catch (e) {
      setError(humanLine(e));
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const save = async () => {
    if (!client || !form) {
      return;
    }
    setBusy(true);
    try {
      const saved = await client.saveEmailChannel(form);
      setChannel(saved);
      setForm(saved.settings);
      toast.show(t("settingsModal.emailChannel.saved"), "success");
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
        toast.show(t("settingsModal.emailChannel.sent"), "success");
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

  const set = (patch: Partial<EmailChannelSettings>) => setForm((f) => (f ? { ...f, ...patch } : f));

  return (
    <Card padding="md" className={mailStyles.root} data-testid="email-channel-card">
      <div className={mailStyles.info}>
        <h2 className={mailStyles.heading}>{t("settingsModal.emailChannel.title")}</h2>
        <p className={formStyles.hint}>{t("settingsModal.emailChannel.description")}</p>
      </div>
      <p className={mailStyles.note}>
        <Info aria-hidden="true" />
        <span>{t("settingsModal.emailChannel.note")}</span>
      </p>
      {error ? (
        <p className={formStyles.error} role="alert">
          {error}
        </p>
      ) : null}
      {form ? (
        <>
          {accounts.length === 0 ? (
            <p className={formStyles.hint}>{t("settingsModal.emailChannel.noAccount")}</p>
          ) : null}
          <label className={formStyles.field}>
            <span className={formStyles.label}>{t("settingsModal.emailChannel.account")}</span>
            <select
              className={formStyles.select}
              value={form.account}
              data-testid="email-channel-account"
              onChange={(e) => set({ account: e.target.value })}
            >
              <option value="">{t("settingsModal.emailChannel.choose")}</option>
              {accounts.map((a) => (
                <option key={a.name} value={a.name}>
                  {a.name} ({a.address})
                </option>
              ))}
            </select>
          </label>
          <label className={formStyles.field}>
            <span className={formStyles.label}>{t("settingsModal.emailChannel.folder")}</span>
            <input
              className={formStyles.input}
              value={form.folder}
              data-testid="email-channel-folder"
              maxLength={200}
              onChange={(e) => set({ folder: e.target.value })}
            />
            <span className={formStyles.hint}>{t("settingsModal.emailChannel.folderHint")}</span>
          </label>
          <label className={formStyles.field}>
            <span className={formStyles.label}>{t("settingsModal.emailChannel.bot")}</span>
            <select
              className={formStyles.select}
              value={form.bot ?? ""}
              data-testid="email-channel-bot"
              onChange={(e) => set({ bot: e.target.value || null })}
            >
              <option value="">{t("settingsModal.emailChannel.choose")}</option>
              {bots.map((b) => (
                <option key={b.id} value={b.id}>
                  {b.name}
                </option>
              ))}
            </select>
          </label>
          <div className={styles.checks}>
            <label className={styles.check}>
              <input
                type="checkbox"
                checked={form.mode === "full"}
                data-testid="email-channel-full"
                onChange={(e) => set({ mode: e.target.checked ? "full" : "summary" })}
              />
              <span>{t("settingsModal.emailChannel.full")}</span>
            </label>
            <label className={styles.check}>
              <input
                type="checkbox"
                checked={form.replies_enabled}
                data-testid="email-channel-replies"
                onChange={(e) => set({ replies_enabled: e.target.checked })}
              />
              <span>{t("settingsModal.emailChannel.replies")}</span>
            </label>
            <label className={styles.check}>
              <input
                type="checkbox"
                checked={form.enabled}
                data-testid="email-channel-enabled"
                onChange={(e) => set({ enabled: e.target.checked })}
              />
              <span>{t("settingsModal.emailChannel.enable")}</span>
            </label>
          </div>
          <div className={mailStyles.actions}>
            <Button size="sm" disabled={busy} data-testid="email-channel-save" onClick={() => void save()}>
              <Mail />
              {t("settingsModal.emailChannel.save")}
            </Button>
            {channel ? (
              <span className={formStyles.hint}>
                {channel.running
                  ? t("settingsModal.emailChannel.running")
                  : t("settingsModal.emailChannel.stopped")}
              </span>
            ) : null}
          </div>
        </>
      ) : null}
      {replies.length > 0 ? (
        <div className={mailStyles.info}>
          <h3 className={mailStyles.heading}>{t("settingsModal.emailChannel.waiting")}</h3>
          {replies.map((r) => (
            <div key={r.id} className={styles.reply} data-testid={`email-reply-${r.id}`}>
              <p className={styles.recipients}>
                {t("settingsModal.emailChannel.to", { to: r.to.join(", ") })}
              </p>
              <p className={formStyles.hint}>
                {t("settingsModal.emailChannel.subject", { subject: r.subject })}
              </p>
              <pre className={styles.body}>{r.body}</pre>
              <div className={mailStyles.actions}>
                <Button
                  size="sm"
                  disabled={busy}
                  data-testid={`email-reply-approve-${r.id}`}
                  onClick={() => void decide(r.id, true)}
                >
                  <Check />
                  {t("settingsModal.emailChannel.approve")}
                </Button>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  data-testid={`email-reply-deny-${r.id}`}
                  onClick={() => void decide(r.id, false)}
                >
                  <X />
                  {t("settingsModal.emailChannel.deny")}
                </Button>
              </div>
            </div>
          ))}
        </div>
      ) : null}
    </Card>
  );
}
