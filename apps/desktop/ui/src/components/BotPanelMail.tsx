import { AtSign, Inbox, Mail, PlugZap, Plus, Send, SlidersHorizontal } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { EmailAccount, EmailMessage, EmailTestReport } from "../api/types";
import { accountsForBot, MAIL_CHANGED_EVENT, mailToolsOn, mailToolsPatch } from "../lib/botMail";
import { humanLine } from "../lib/humanError";
import { mailboxStatus } from "../lib/mailboxStatus";
import { useDaemon } from "../state/DaemonContext";
import { TestResult } from "../views/settings/EmailCard";
import { Button } from "ui";
import { PanelGroup, StatusChip, type StatusTone } from "./PanelRows";
import { Switch } from "./SettingsUi";
import { useToast } from "./Toast";
import styles from "./BotPanelMail.module.css";

function MailboxRow({ account, botName }: { account: EmailAccount; botName: string }) {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const [report, setReport] = useState<EmailTestReport | null>(null);
  const [checkedAt, setCheckedAt] = useState<Date | null>(null);
  const [checking, setChecking] = useState(false);
  const agentmail = account.provider === "agentmail";
  const [messages, setMessages] = useState<EmailMessage[] | null>(null);
  const [loadingMessages, setLoadingMessages] = useState(false);
  const [sending, setSending] = useState(false);

  const toggleMessages = async () => {
    if (messages !== null) {
      setMessages(null);
      return;
    }
    if (!client) return;
    setLoadingMessages(true);
    try {
      setMessages(await client.listEmailMessages(account.name, 10));
    } catch (e) {
      toast.show(t("botPanel.mail.recentError", { message: humanLine(e) }), "danger");
    } finally {
      setLoadingMessages(false);
    }
  };

  const sendTest = async () => {
    if (!client) return;
    setSending(true);
    try {
      const sent = await client.sendEmailTest(account.name);
      toast.show(t("botPanel.mail.testSent", { address: sent.sent_to }), "success");
    } catch (e) {
      toast.show(t("botPanel.mail.testError", { message: humanLine(e) }), "danger");
    } finally {
      setSending(false);
    }
  };

  const check = async () => {
    if (!client) return;
    setChecking(true);
    try {
      setReport(await client.testEmailAccount(account.name));
      setCheckedAt(new Date());
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setChecking(false);
    }
  };

  const state = mailboxStatus(account, { report });
  const checkedTime = checkedAt
    ? checkedAt.toLocaleTimeString(i18n.language, { hour: "2-digit", minute: "2-digit" })
    : null;
  const status: { tone: StatusTone; label: string } =
    state.tone === "attention"
      ? state.reason === "secretMissing"
        ? { tone: "warn", label: t("panelTabs.mail.status.secretMissing") }
        : { tone: "danger", label: t("panelTabs.mail.status.problem") }
      : state.tested && checkedTime
        ? { tone: "ok", label: t("botPanel.mail.checked", { time: checkedTime }) }
        : { tone: "muted", label: t("panelTabs.mail.status.untested") };
  const scope = [agentmail ? t("botPanel.mail.agentmail") : null, account.bot ? t("botPanel.mail.own", { name: botName }) : t("botPanel.mail.shared")]
    .filter(Boolean)
    .join(" · ");

  return (
    <li className={styles.box} data-testid={`bot-mail-${account.name}`}>
      <div className={styles.address}>
        {agentmail ? <AtSign aria-hidden="true" /> : <Mail aria-hidden="true" />}
        <span className={styles.addressText} title={account.address}>
          {account.address}
        </span>
      </div>
      <span className={styles.scope}>{scope}</span>
      <div className={styles.top}>
        <StatusChip tone={status.tone}>{status.label}</StatusChip>
        <Button
          size="sm"
          variant="secondary"
          disabled={checking}
          data-testid={`bot-mail-check-${account.name}`}
          onClick={() => void check()}
        >
          <PlugZap aria-hidden="true" />
          {checking ? t("botPanel.mail.checking") : t("botPanel.mail.check")}
        </Button>
      </div>
      {report && !report.ok ? <TestResult report={report} /> : null}
      <details className={styles.more}>
        <summary>{t("panelTabs.mail.more")}</summary>
        <div className={styles.row}>
          <button
            type="button"
            className={styles.textButton}
            disabled={loadingMessages}
            aria-expanded={messages !== null}
            data-testid={`bot-mail-recent-${account.name}`}
            onClick={() => void toggleMessages()}
          >
            <Inbox aria-hidden="true" />
            {messages !== null ? t("botPanel.mail.hideRecent") : t("botPanel.mail.recent")}
          </button>
          <button
            type="button"
            className={styles.textButton}
            disabled={sending}
            title={t("botPanel.mail.sendTestHint", { address: account.address })}
            data-testid={`bot-mail-send-test-${account.name}`}
            onClick={() => void sendTest()}
          >
            <Send aria-hidden="true" />
            {sending ? t("botPanel.mail.sendingTest") : t("botPanel.mail.sendTest")}
          </button>
        </div>
        {messages !== null ? (
          messages.length === 0 ? (
            <p className={styles.muted}>{t("botPanel.mail.noMessages")}</p>
          ) : (
            <ul className={styles.messages} data-testid={`bot-mail-messages-${account.name}`}>
              {messages.map((m) => (
                <li key={m.id} className={styles.message} data-unread={m.unread ? "true" : undefined}>
                  <span className={styles.subject}>{m.subject || t("botPanel.mail.noSubject")}</span>
                  <span className={styles.from}>
                    {m.from}
                    {m.date ? ` · ${new Date(m.date).toLocaleDateString(i18n.language)}` : ""}
                  </span>
                </li>
              ))}
            </ul>
          )
        ) : null}
      </details>
    </li>
  );
}

/**
 * Mail in the right panel: the mailboxes this bot may use, whether it is offered the mail tools,
 * and a way to check the connection.  Adding or removing a mailbox happens in Settings, Mail.
 */
export function BotPanelMail({
  botId,
  botName,
  onOpenMail
}: {
  botId: string;
  botName: string;
  onOpenMail: () => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, bots, refreshBots } = useDaemon();
  const bot = bots.find((b) => b.id === botId);
  const [accounts, setAccounts] = useState<EmailAccount[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const load = useCallback(async () => {
    if (!client) return;
    try {
      setAccounts(await client.listEmailAccounts());
      setError(null);
    } catch (e) {
      setError(humanLine(e));
    }
  }, [client]);

  useEffect(() => {
    setAccounts(null);
    void load();
    window.addEventListener(MAIL_CHANGED_EVENT, load);
    return () => window.removeEventListener(MAIL_CHANGED_EVENT, load);
  }, [load, botId]);

  const mine = accounts ? accountsForBot(accounts, botId) : [];
  const toolsOn = bot ? mailToolsOn(bot.config) : false;

  const setTools = async (on: boolean) => {
    if (!client || !bot) return;
    const patch = mailToolsPatch(bot, on);
    if (patch.ops.length === 0) return;
    setSaving(true);
    try {
      await client.patchBot(bot.id, patch);
      await refreshBots();
    } catch (e) {
      toast.show(t("botPanel.mail.toolsError", { message: humanLine(e) }), "danger");
    } finally {
      setSaving(false);
    }
  };

  return (
    <PanelGroup
      title={t("botPanel.mail.heading")}
      testId="bot-panel-mail"
      action={
        <button
          type="button"
          className={styles.tool}
          title={t("botPanel.mail.manage")}
          aria-label={t("botPanel.mail.manage")}
          data-testid="bot-panel-mail-manage"
          onClick={onOpenMail}
        >
          <SlidersHorizontal aria-hidden="true" />
        </button>
      }
    >
      {error ? (
        <p className={styles.error} role="alert">
          {t("botPanel.mail.loadError", { message: error })}
        </p>
      ) : accounts === null ? (
        <p className={styles.muted}>{t("botPanel.mail.loading")}</p>
      ) : mine.length === 0 ? (
        <div className={styles.empty}>
          <p className={styles.muted}>{t("botPanel.mail.empty", { name: botName })}</p>
          <Button size="sm" variant="primary" data-testid="bot-panel-mail-connect" onClick={onOpenMail}>
            <Plus aria-hidden="true" />
            {t("botPanel.mail.connect")}
          </Button>
        </div>
      ) : (
        <>
          <ul className={styles.list}>
            {mine.map((a) => (
              <MailboxRow key={a.name} account={a} botName={botName} />
            ))}
          </ul>
          <div className={styles.access}>
            <div className={styles.row}>
              <span className={styles.switchLabel} title={toolsOn ? t("botPanel.mail.toolsOnHint") : undefined}>
                {t("botPanel.mail.toolsLabel", { name: botName })}
              </span>
              <Switch
                checked={toolsOn}
                disabled={saving || !bot}
                label={t("botPanel.mail.toolsLabel", { name: botName })}
                onChange={(on) => void setTools(on)}
              />
            </div>
            {toolsOn ? null : (
              <p className={styles.muted} data-testid="bot-mail-tools-hint">
                {t("botPanel.mail.toolsOffHint", { name: botName })}
              </p>
            )}
          </div>
        </>
      )}
    </PanelGroup>
  );
}
