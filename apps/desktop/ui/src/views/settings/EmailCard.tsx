import {
  CircleAlert,
  CircleCheck,
  Info,
  Mail,
  Pencil,
  PlugZap,
  Plus,
  Trash2,
  Users
} from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button, Card } from "ui";
import { BrandIcon } from "../../components/BrandIcon";
import { botView } from "../../lib/botProfile";
import type { EmailAccount, EmailTestReport } from "../../api/types";
import { BotAccessPicker } from "../../components/BotAccessPicker";
import formStyles from "../../components/Form.module.css";
import { Segmented } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import {
  accountBotName,
  applyPreset,
  EMAIL_PRESETS,
  type EmailForm,
  type EmailFormError,
  type EmailPresetId,
  emptyForm,
  formFromAccount,
  probeTone,
  toInput,
  toPatch,
  validateForm
} from "../../lib/emailAccounts";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./EmailCard.module.css";

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const PRESET_IDS: readonly EmailPresetId[] = [
  ...EMAIL_PRESETS.map((p) => p.id),
  "custom"
];

/** One line per server: "IMAP: Connected", "SMTP: Login rejected". */
function TestResult({ report }: { report: EmailTestReport }) {
  const { t } = useTranslation();
  return (
    <ul className={styles.result} data-testid="email-test-result">
      {(["imap", "smtp"] as const).map((k) => {
        const tone = probeTone(report[k]);
        return (
          <li
            key={k}
            className={[styles.pill, tone === "ok" ? styles.pillOk : styles.pillBad].join(" ")}
          >
            {tone === "ok" ? <CircleCheck aria-hidden="true" /> : <CircleAlert aria-hidden="true" />}
            <span>
              {t(`settingsModal.email.result.${k}`)}: {t(`settingsModal.email.result.${report[k]}`)}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

function Field({
  id,
  label,
  hint,
  error,
  children
}: {
  id: string;
  label: string;
  hint?: string;
  error?: string | null;
  children: React.ReactNode;
}) {
  return (
    <div className={formStyles.field}>
      <label className={formStyles.label} htmlFor={id}>
        {label}
      </label>
      {children}
      {error ? <p className={formStyles.error}>{error}</p> : null}
      {hint && !error ? <p className={formStyles.hint}>{hint}</p> : null}
    </div>
  );
}

function AccountForm({
  existing,
  original,
  onDone
}: {
  existing: string[];
  /** The account being edited; `undefined` adds a new one. */
  original?: EmailAccount;
  onDone: () => Promise<void>;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, bots } = useDaemon();
  const editing = original !== undefined;
  const [form, setForm] = useState<EmailForm>(() =>
    original ? formFromAccount(original) : emptyForm()
  );
  const [errors, setErrors] = useState<EmailFormError[]>([]);
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<EmailTestReport | null>(null);
  const [testing, setTesting] = useState(false);
  const set = (patch: Partial<EmailForm>) => {
    setForm((f) => ({ ...f, ...patch }));
    setReport(null);
  };
  const err = (k: EmailFormError) =>
    errors.includes(k) ? t(`settingsModal.email.errors.${k}`) : null;
  const check = () => {
    const found = validateForm(form, { existing, editing, original });
    setErrors(found);
    return found.length === 0;
  };

  const test = async () => {
    if (!client || !check()) {
      return;
    }
    setTesting(true);
    try {
      setReport(await client.testEmailDraft(toInput(form)));
    } catch (e) {
      toast.show(errorText(e), "danger");
    } finally {
      setTesting(false);
    }
  };

  const submit = async () => {
    if (!client || !check()) {
      return;
    }
    setBusy(true);
    try {
      if (original) {
        await client.updateEmailAccount(original.name, toPatch(form, original));
        toast.show(t("settingsModal.email.toast.saved", { name: original.name }), "success");
      } else {
        await client.createEmailAccount(toInput(form));
        toast.show(t("settingsModal.email.toast.added", { name: form.name.trim() }), "success");
      }
      await onDone();
    } catch (e) {
      toast.show(errorText(e), "danger");
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      className={styles.form}
      data-testid="email-form"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <div className={formStyles.field}>
        <span className={formStyles.label}>{t("settingsModal.email.form.preset")}</span>
        <div className={styles.presets} role="group" aria-label={t("settingsModal.email.form.preset")}>
          {PRESET_IDS.map((id) => (
            <Button
              key={id}
              size="sm"
              variant="secondary"
              active={form.preset === id}
              aria-pressed={form.preset === id}
              data-testid={`email-preset-${id}`}
              onClick={() => set(applyPreset(form, id))}
            >
              {t(`settingsModal.email.form.presets.${id}`)}
            </Button>
          ))}
        </div>
      </div>

      <div className={styles.grid}>
        <Field
          id="email-name"
          label={t("settingsModal.email.form.name")}
          hint={t("settingsModal.email.form.nameHint")}
          error={err("name")}
        >
          <input
            id="email-name"
            className={formStyles.input}
            value={form.name}
            disabled={editing}
            maxLength={64}
            autoComplete="off"
            placeholder="work"
            onChange={(e) => set({ name: e.target.value })}
          />
        </Field>
        <Field
          id="email-address"
          label={t("settingsModal.email.form.address")}
          error={err("address")}
        >
          <input
            id="email-address"
            className={formStyles.input}
            type="email"
            value={form.address}
            autoComplete="off"
            placeholder="you@example.com"
            onChange={(e) => set({ address: e.target.value })}
          />
        </Field>
      </div>

      <div className={styles.grid}>
        <Field
          id="email-imap-host"
          label={t("settingsModal.email.form.imapHost")}
          error={err("imapHost")}
        >
          <input
            id="email-imap-host"
            className={formStyles.input}
            value={form.imapHost}
            autoComplete="off"
            placeholder="imap.example.com"
            onChange={(e) => set({ imapHost: e.target.value, preset: "custom" })}
          />
        </Field>
        <Field
          id="email-imap-port"
          label={t("settingsModal.email.form.port")}
          error={err("imapPort")}
        >
          <input
            id="email-imap-port"
            className={`${formStyles.input} ${styles.port}`}
            inputMode="numeric"
            value={form.imapPort}
            onChange={(e) => set({ imapPort: e.target.value, preset: "custom" })}
          />
        </Field>
      </div>

      <div className={styles.grid}>
        <Field
          id="email-smtp-host"
          label={t("settingsModal.email.form.smtpHost")}
          error={err("smtpHost")}
        >
          <input
            id="email-smtp-host"
            className={formStyles.input}
            value={form.smtpHost}
            autoComplete="off"
            placeholder="smtp.example.com"
            onChange={(e) => set({ smtpHost: e.target.value, preset: "custom" })}
          />
        </Field>
        <Field
          id="email-smtp-port"
          label={t("settingsModal.email.form.port")}
          error={err("smtpPort")}
        >
          <input
            id="email-smtp-port"
            className={`${formStyles.input} ${styles.port}`}
            inputMode="numeric"
            value={form.smtpPort}
            onChange={(e) => set({ smtpPort: e.target.value, preset: "custom" })}
          />
        </Field>
      </div>
      <div className={formStyles.field}>
        <span className={formStyles.label}>{t("settingsModal.email.form.security")}</span>
        <Segmented
          label={t("settingsModal.email.form.security")}
          value={form.smtpSecurity}
          onChange={(smtpSecurity) => set({ smtpSecurity, preset: "custom" })}
          options={[
            { value: "implicit", label: t("settingsModal.email.form.implicit") },
            { value: "start_tls", label: t("settingsModal.email.form.startTls") }
          ]}
        />
      </div>

      <div className={styles.grid}>
        <Field
          id="email-username"
          label={t("settingsModal.email.form.username")}
          hint={t("settingsModal.email.form.usernameHint")}
        >
          <input
            id="email-username"
            className={formStyles.input}
            value={form.username}
            autoComplete="off"
            onChange={(e) => set({ username: e.target.value })}
          />
        </Field>
        <Field
          id="email-password"
          label={t("settingsModal.email.form.password")}
          hint={
            editing
              ? `${t("settingsModal.email.form.passwordKeep")} ${t("settingsModal.email.form.passwordHint")}`
              : t("settingsModal.email.form.passwordHint")
          }
          error={err("password")}
        >
          <input
            id="email-password"
            className={formStyles.input}
            type="password"
            value={form.password}
            autoComplete="new-password"
            onChange={(e) => set({ password: e.target.value })}
          />
        </Field>
      </div>

      <div className={formStyles.field}>
        <span className={formStyles.label}>{t("settingsModal.email.form.access.label")}</span>
        <BotAccessPicker
          single
          scope={form.bot}
          bots={bots}
          onChange={(bot) => set({ bot })}
          texts={{
            label: t("settingsModal.email.form.access.label"),
            all: t("settingsModal.email.form.access.all"),
            some: t("settingsModal.email.form.access.some"),
            nobody: t("settingsModal.email.form.access.nobody")
          }}
          allWarning={t("settingsModal.email.form.access.allWarning")}
        />
      </div>

      <Field
        id="email-senders"
        label={t("settingsModal.email.form.senders")}
        hint={t("settingsModal.email.form.sendersHint")}
        error={err("senders")}
      >
        <textarea
          id="email-senders"
          className={formStyles.textarea}
          value={form.allowedSenders}
          placeholder="you@example.com"
          onChange={(e) => set({ allowedSenders: e.target.value })}
        />
      </Field>

      {report ? <TestResult report={report} /> : null}

      <div className={styles.actions}>
        <Button type="submit" size="sm" variant="primary" disabled={busy || testing}>
          {editing ? t("settingsModal.email.form.save") : t("settingsModal.email.form.create")}
        </Button>
        <Button
          size="sm"
          variant="secondary"
          disabled={busy || testing}
          data-testid="email-test-draft"
          onClick={() => void test()}
        >
          <PlugZap />
          {testing ? t("settingsModal.email.testing") : t("settingsModal.email.test")}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => void onDone()}>
          {t("common.cancel")}
        </Button>
      </div>
    </form>
  );
}

function AccountRow({
  account,
  busy,
  onEdit,
  onDelete,
  onTest
}: {
  account: EmailAccount;
  busy: boolean;
  onEdit: () => void;
  onDelete: () => void;
  onTest: () => Promise<EmailTestReport | null>;
}) {
  const { t } = useTranslation();
  const { bots } = useDaemon();
  const [confirm, setConfirm] = useState(false);
  const [testing, setTesting] = useState(false);
  const [report, setReport] = useState<EmailTestReport | null>(null);
  const botName = accountBotName(account, bots, t("settingsModal.email.deletedBot"));
  const boundBot = account.bot ? bots.find((b) => b.id === account.bot) : undefined;
  return (
    <li className={styles.row} data-testid={`email-account-${account.name}`}>
      {/gmail|google/i.test(account.imap.host) ? (
        <BrandIcon name="gmail" />
      ) : (
        <span className={styles.icon} aria-hidden="true">
          <Mail />
        </span>
      )}
      <div className={styles.main}>
        <div className={styles.title}>
          <span className={styles.name}>{account.name}</span>
          <span className={styles.address}>{account.address}</span>
          <span className={styles.tag}>
            {boundBot ? <BotAvatar bot={botView(boundBot)} size="xs" decorative /> : <Users aria-hidden="true" />}
            {botName === null
              ? t("settingsModal.email.allBots")
              : t("settingsModal.email.onlyBot", { name: botName })}
          </span>
        </div>
        <div className={styles.meta}>
          <span>{account.imap.host}</span>
          <span aria-hidden="true">·</span>
          <span>{account.smtp.host}</span>
          {account.password_set ? null : (
            <>
              <span aria-hidden="true">·</span>
              <span className={styles.missing}>{t("settingsModal.email.passwordMissing")}</span>
            </>
          )}
        </div>
        {report ? <TestResult report={report} /> : null}
      </div>
      <div className={styles.rowActions}>
        <Button
          size="sm"
          variant="ghost"
          disabled={busy || testing}
          data-testid={`email-test-${account.name}`}
          onClick={() => {
            setTesting(true);
            void onTest()
              .then(setReport)
              .finally(() => setTesting(false));
          }}
        >
          <PlugZap />
          {testing ? t("settingsModal.email.testing") : t("settingsModal.email.test")}
        </Button>
        <Button size="sm" variant="ghost" disabled={busy} onClick={onEdit}>
          <Pencil />
          {t("settingsModal.email.edit")}
        </Button>
        {confirm ? (
          <>
            <Button
              size="sm"
              variant="danger"
              disabled={busy}
              data-testid={`email-delete-confirm-${account.name}`}
              onClick={() => {
                setConfirm(false);
                onDelete();
              }}
            >
              {t("settingsModal.email.deleteConfirm")}
            </Button>
            <Button size="sm" variant="ghost" onClick={() => setConfirm(false)}>
              {t("common.cancel")}
            </Button>
          </>
        ) : (
          <Button
            size="sm"
            variant="ghost"
            disabled={busy}
            data-testid={`email-delete-${account.name}`}
            onClick={() => setConfirm(true)}
          >
            <Trash2 />
            {t("settingsModal.email.delete")}
          </Button>
        )}
      </div>
    </li>
  );
}

/**
 * Settings card "E-mail": the mailboxes bots can use, each optionally bound to
 * one bot.  Passwords are write-only; the daemon never returns them.
 */
export function EmailCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const [accounts, setAccounts] = useState<EmailAccount[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [mode, setMode] = useState<"list" | "add" | { edit: string }>("list");
  const [busy, setBusy] = useState(false);

  const reload = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      setAccounts(await client.listEmailAccounts());
      setLoadError(null);
    } catch (e) {
      setLoadError(errorText(e));
    }
  }, [client]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const remove = async (name: string) => {
    if (!client) {
      return;
    }
    setBusy(true);
    try {
      await client.deleteEmailAccount(name);
      toast.show(t("settingsModal.email.toast.deleted", { name }), "success");
      await reload();
    } catch (e) {
      toast.show(errorText(e), "danger");
    } finally {
      setBusy(false);
    }
  };

  const test = async (name: string): Promise<EmailTestReport | null> => {
    if (!client) {
      return null;
    }
    try {
      return await client.testEmailAccount(name);
    } catch (e) {
      toast.show(errorText(e), "danger");
      return null;
    }
  };

  const list = accounts ?? [];
  const editing = typeof mode === "object" ? list.find((a) => a.name === mode.edit) : undefined;

  return (
    <Card padding="md" className={styles.root} data-testid="email-card">
      <div className={styles.info}>
        <h2 className={styles.heading}>{t("settingsModal.email.title")}</h2>
        <p className={formStyles.hint}>{t("settingsModal.email.description")}</p>
      </div>
      <p className={styles.note}>
        <Info aria-hidden="true" />
        <span>{t("settingsModal.email.note")}</span>
      </p>
      {loadError ? (
        <p className={formStyles.error}>
          {t("settingsModal.email.loadError", { message: loadError })}
        </p>
      ) : null}
      {accounts && list.length === 0 && mode === "list" ? (
        <p className={formStyles.hint}>{t("settingsModal.email.empty")}</p>
      ) : null}
      {list.length > 0 ? (
        <ul className={styles.list}>
          {list.map((a) => (
            <AccountRow
              key={a.name}
              account={a}
              busy={busy}
              onEdit={() => setMode({ edit: a.name })}
              onDelete={() => void remove(a.name)}
              onTest={() => test(a.name)}
            />
          ))}
        </ul>
      ) : null}
      {mode === "list" ? (
        <div className={styles.actions}>
          <Button
            size="sm"
            variant="secondary"
            data-testid="email-add"
            onClick={() => setMode("add")}
          >
            <Plus />
            {t("settingsModal.email.add")}
          </Button>
        </div>
      ) : (
        <AccountForm
          key={typeof mode === "object" ? mode.edit : "add"}
          existing={list.map((a) => a.name)}
          original={editing}
          onDone={async () => {
            setMode("list");
            await reload();
          }}
        />
      )}
    </Card>
  );
}
