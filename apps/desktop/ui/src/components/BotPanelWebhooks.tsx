import { Webhook } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { IncomingWebhook, IncomingWebhookCreated } from "../api/types";
import { humanLine } from "../lib/humanError";
import { webhookAddress } from "../lib/incomingWebhooks";
import { useDaemon } from "../state/DaemonContext";
import { Switch } from "./SettingsUi";
import { useToast } from "./Toast";
import styles from "./BotPanelWebhooks.module.css";

/**
 * Automation tab: webhooks that start this bot. The secret is shown once, at
 * creation. Calls stay on the daemon's existing address; the note tells the
 * owner how to reach it through a tunnel they choose.
 */
export function BotPanelWebhooks({ botId }: { botId: string }) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const [rows, setRows] = useState<IncomingWebhook[] | null>(null);
  const [name, setName] = useState("");
  const [instruction, setInstruction] = useState("");
  const [busy, setBusy] = useState(false);
  const [created, setCreated] = useState<IncomingWebhookCreated | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [testingId, setTestingId] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!client) return;
    try {
      setRows(await client.listIncomingWebhooks(botId));
      setError(null);
    } catch (e) {
      setError(`${t("botPanel.webhooks.loadError")} ${humanLine(e)}`);
    }
  }, [botId, client, t]);

  useEffect(() => {
    setCreated(null);
    void load();
  }, [load]);

  const create = async () => {
    if (!client || busy) return;
    setBusy(true);
    try {
      const made = await client.createIncomingWebhook(botId, {
        name: name.trim(),
        instruction: instruction.trim() || undefined
      });
      setCreated(made);
      setName("");
      setInstruction("");
      await load();
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
    }
  };

  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      toast.show(t("botPanel.webhooks.copied"), "success");
    } catch (e) {
      toast.show(humanLine(e), "danger");
    }
  };

  const address = client && created ? webhookAddress(client.baseUrl, created.webhook.id) : "";

  return (
    <section className={styles.section} aria-labelledby="bot-webhooks-title">
      <div className={styles.head}>
        <h3 className={styles.title} id="bot-webhooks-title">
          <Webhook aria-hidden="true" /> {t("botPanel.webhooks.title")}
        </h3>
      </div>
      <p className={styles.intro}>{t("botPanel.webhooks.intro")}</p>
      <p className={styles.note}>{t("botPanel.webhooks.tunnel")}</p>
      <form
        className={styles.form}
        onSubmit={(e) => {
          e.preventDefault();
          void create();
        }}
      >
        <label className={styles.field}>
          {t("botPanel.webhooks.name")}
          <input
            value={name}
            maxLength={80}
            required
            placeholder={t("botPanel.webhooks.namePlaceholder")}
            aria-label={t("botPanel.webhooks.name")}
            data-testid="webhook-name"
            onChange={(e) => setName(e.target.value)}
          />
        </label>
        <label className={styles.field}>
          {t("botPanel.webhooks.instruction")}
          <textarea
            value={instruction}
            maxLength={400}
            aria-label={t("botPanel.webhooks.instruction")}
            data-testid="webhook-instruction"
            onChange={(e) => setInstruction(e.target.value)}
          />
        </label>
        <p className={styles.note}>{t("botPanel.webhooks.instructionHint")}</p>
        <button type="submit" className={styles.primary} disabled={busy || name.trim() === ""} data-testid="webhook-create">
          {busy ? t("botPanel.webhooks.creating") : t("botPanel.webhooks.create")}
        </button>
      </form>
      {created ? (
        <div className={styles.card} data-testid="webhook-secret-once">
          <p className={styles.note}>{t("botPanel.webhooks.secretLabel")}</p>
          <p className={styles.note}>{t("botPanel.webhooks.secretOnce")}</p>
          <p className={styles.secret}>{created.secret}</p>
          <div className={styles.row}>
            <button type="button" className={styles.quiet} onClick={() => void copy(created.secret)}>
              {t("botPanel.webhooks.copySecret")}
            </button>
          </div>
          <p className={styles.note}>{t("botPanel.webhooks.urlLabel")}</p>
          <p className={styles.url}>{address}</p>
          <div className={styles.row}>
            <button type="button" className={styles.quiet} onClick={() => void copy(address)}>
              {t("botPanel.webhooks.copyUrl")}
            </button>
          </div>
          <p className={styles.note}>{t("botPanel.webhooks.sign")}</p>
        </div>
      ) : null}
      {error ? <p className={styles.error} role="alert">{error}</p> : null}
      {rows === null ? null : rows.length === 0 ? (
        <p className={styles.empty}>{t("botPanel.webhooks.empty")}</p>
      ) : (
        <ul className={styles.list}>
          {rows.map((row) => (
            <li key={row.id} className={styles.card} data-testid={`webhook-${row.id}`}>
              <div className={styles.cardHead}>
                <h4 className={styles.name}>{row.name}</h4>
                <Switch
                  checked={row.enabled}
                  label={row.enabled ? t("botPanel.webhooks.enabled") : t("botPanel.webhooks.off")}
                  onChange={(enabled) => {
                    if (!client) return;
                    void client
                      .updateIncomingWebhook(botId, row.id, { enabled })
                      .then(() => load())
                      .catch((e: unknown) => toast.show(humanLine(e), "danger"));
                  }}
                />
              </div>
              <p className={styles.url}>{client ? webhookAddress(client.baseUrl, row.id) : row.path}</p>
              <div className={styles.row}>
                <button
                  type="button"
                  className={styles.quiet}
                  disabled={!row.enabled || testingId === row.id}
                  data-testid={`webhook-test-${row.id}`}
                  onClick={() => {
                    if (!client || testingId) return;
                    setTestingId(row.id);
                    void client
                      .testIncomingWebhook(botId, row.id)
                      .then(() => toast.show(t("botPanel.webhooks.testStarted"), "success"))
                      .catch((e: unknown) => toast.show(humanLine(e), "danger"))
                      .finally(() => setTestingId(null));
                  }}
                >
                  {testingId === row.id ? t("botPanel.webhooks.testing") : t("botPanel.webhooks.test")}
                </button>
                <button
                  type="button"
                  className={styles.danger}
                  onClick={() => {
                    if (!client) return;
                    void client
                      .deleteIncomingWebhook(botId, row.id)
                      .then(() => {
                        if (created?.webhook.id === row.id) setCreated(null);
                        toast.show(t("botPanel.webhooks.removed"), "success");
                        return load();
                      })
                      .catch((e: unknown) => toast.show(humanLine(e), "danger"));
                  }}
                >
                  {t("botPanel.webhooks.remove")}
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
