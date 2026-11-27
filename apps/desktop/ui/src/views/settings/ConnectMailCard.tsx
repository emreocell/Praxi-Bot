import { CalendarDays, CircleCheck, Mail, Unplug } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { SettingsCard } from "../../components/SettingsUi";
import { MAIL_CHANGED_EVENT } from "../../lib/botMail";
import { humanLine } from "../../lib/humanError";
import { type Connected, connectedCount, disconnectAll, loadConnected } from "../../lib/mailGuide";
import { useDaemon } from "../../state/DaemonContext";
import { useToast } from "../../components/Toast";
import { ConnectMailModal } from "../ConnectMail";
import formStyles from "../../components/Form.module.css";
import styles from "../ConnectMail.module.css";

const K = "settingsModal.connectMail";

/**
 * Settings card "Connect my email and calendar": the one place to start, with what is connected
 * now and a "Disconnect everything" that removes it all (and the saved passwords) after one
 * confirmation. The wizard itself is `ConnectMailFlow`.
 */
export function ConnectMailCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, bots } = useDaemon();
  const [open, setOpen] = useState(false);
  const [connected, setConnected] = useState<Connected | null>(null);
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    if (!client) return;
    try {
      setConnected(await loadConnected(client));
      setError(null);
    } catch (e) {
      setError(humanLine(e));
    }
  }, [client]);

  useEffect(() => {
    void reload();
    // Another card added or removed a mailbox: the numbers here follow.
    window.addEventListener(MAIL_CHANGED_EVENT, reload);
    return () => window.removeEventListener(MAIL_CHANGED_EVENT, reload);
  }, [reload]);

  const count = connected ? connectedCount(connected) : 0;

  const disconnect = async () => {
    if (!client || !connected) return;
    setBusy(true);
    try {
      const result = await disconnectAll(client, connected);
      if (result.failed.length > 0) {
        toast.show(t(`${K}.card.disconnectFailed`, { names: result.failed.join(", ") }), "danger");
      } else {
        toast.show(t(`${K}.card.disconnected`, { count: result.removed }), "success");
      }
      window.dispatchEvent(new Event(MAIL_CHANGED_EVENT));
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setConfirm(false);
      setBusy(false);
      await reload();
    }
  };

  return (
    <SettingsCard title={t(`${K}.card.title`)} description={t(`${K}.card.description`)} testId="connect-mail-card">
      <div className={styles.card}>
        {error ? (
          <p className={formStyles.error} role="alert">
            {error}
          </p>
        ) : null}
        <div className={styles.tags} data-testid="connect-mail-counts">
          {connected && count > 0 ? (
            <>
              <span className={styles.tag}>
                <Mail aria-hidden="true" />
                {t(`${K}.card.mailboxes`, { count: connected.mailboxes.length })}
              </span>
              <span className={styles.tag}>
                <CalendarDays aria-hidden="true" />
                {t(`${K}.card.calendars`, { count: connected.calendars.length })}
              </span>
              {connected.googleApps.length > 0 ? (
                <span className={styles.tag}>
                  <CircleCheck aria-hidden="true" />
                  {t(`${K}.card.googleApps`, { count: connected.googleApps.length })}
                </span>
              ) : null}
            </>
          ) : connected ? (
            <span className={formStyles.hint}>{t(`${K}.card.none`)}</span>
          ) : null}
        </div>
        <div className={styles.row}>
          <Button size="sm" variant="primary" data-testid="connect-mail-start" onClick={() => setOpen(true)}>
            {t(`${K}.card.start`)}
          </Button>
          {count > 0 && !confirm ? (
            <Button
              size="sm"
              variant="ghost"
              disabled={busy}
              data-testid="connect-mail-disconnect"
              onClick={() => setConfirm(true)}
            >
              <Unplug />
              {t(`${K}.card.disconnect`)}
            </Button>
          ) : null}
        </div>
        {confirm ? (
          <div className={styles.card} data-testid="connect-mail-disconnect-confirm">
            <p className={styles.text}>{t(`${K}.card.disconnectHint`)}</p>
            <div className={styles.row}>
              <Button
                size="sm"
                variant="danger"
                disabled={busy}
                data-testid="connect-mail-disconnect-yes"
                onClick={() => void disconnect()}
              >
                {t(`${K}.card.disconnectYes`)}
              </Button>
              <Button size="sm" variant="ghost" disabled={busy} onClick={() => setConfirm(false)}>
                {t("common.cancel")}
              </Button>
            </div>
          </div>
        ) : null}
      </div>
      <ConnectMailModal
        open={open}
        client={client}
        bots={bots}
        onClose={() => setOpen(false)}
        onChanged={() => void reload()}
      />
    </SettingsCard>
  );
}
