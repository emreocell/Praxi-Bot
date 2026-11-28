import { ChevronDown, Globe, KeyRound, LogIn, TerminalSquare, TriangleAlert } from "lucide-react";
import { type ReactNode, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { BotAccessPicker } from "../components/BotAccessPicker";
import { ConnectionTestButton } from "../components/ConnectionTest";
import formStyles from "../components/Form.module.css";
import { McpTrustChoice } from "../components/McpTrustChoice";
import type { ConnectionReportLike } from "../lib/connectionTest";
import { humanLine } from "../lib/humanError";
import { ApiError } from "../api/client";
import {
  type CustomMcpDraft,
  type DraftField,
  type DraftPlan,
  type McpAuthMode,
  draftPlan,
  emptyDraft,
  isDraftValid,
  suggestName,
  validateDraft
} from "../lib/customMcp";
import { type BotScope, botSelection } from "../lib/mcpApps";
import { useDaemon } from "../state/DaemonContext";
import styles from "./CustomMcpForm.module.css";

/**
 * "Add your own MCP server": a server address first (with an optional access key header or a
 * browser sign-in), a local command as the folded-away alternative. The server is saved switched
 * off; "Test connection" saves it, turns it on, asks it for its tools and turns it off again when
 * that fails. Keys go to the secret store; the settings only name them.
 */
export function CustomMcpForm({
  existing,
  onDone,
  onCancel,
  onSignIn
}: {
  existing: string[];
  /** The server was saved (`on`: it was tested and left on). */
  onDone: (name: string, on: boolean) => Promise<void> | void;
  onCancel: () => void;
  /** Start the browser sign-in of a saved server. */
  onSignIn: (name: string) => void;
}) {
  const { t } = useTranslation();
  const { client, bots } = useDaemon();
  const id = useId();
  const [draft, setDraft] = useState<CustomMcpDraft>(emptyDraft);
  const [nameEdited, setNameEdited] = useState(false);
  const [touched, setTouched] = useState<Partial<Record<DraftField, true>>>({});
  const [submitted, setSubmitted] = useState(false);
  const [scope, setScope] = useState<BotScope>({ mode: "all" });
  const [trust, setTrust] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // A server this form already saved (by a test): later saves update it, Cancel removes it.
  const [saved, setSaved] = useState<string | null>(null);
  const [testedOn, setTestedOn] = useState(false);
  // The test was answered with "sign in first" and the browser sign-in runs: the server stays on for it.
  const [signingIn, setSigningIn] = useState(false);
  // Remounts the test button, so a result never describes settings that were changed since.
  const [testRun, setTestRun] = useState(0);
  const formRef = useRef<HTMLFormElement>(null);

  const others = useMemo(() => existing.filter((n) => n !== saved), [existing, saved]);
  const check = useMemo(() => validateDraft(draft, others), [draft, others]);
  const valid = isDraftValid(check);
  const shown = (field: DraftField) => (submitted || touched[field] ? check.errors[field] : undefined);
  const touch = (field: DraftField) => setTouched((m) => ({ ...m, [field]: true }));

  const change = (patch: Partial<CustomMcpDraft>) => {
    setDraft((d) => {
      const next = { ...d, ...patch };
      // The name follows the address until the owner types one (and never after it was saved).
      if (patch.url !== undefined && !nameEdited && !saved) {
        next.name = suggestName(patch.url, others);
      }
      return next;
    });
    invalidate();
  };
  /** Any change after a test: the next Add saves again (switched off) and the old result goes. */
  const invalidate = () => {
    setTestedOn(false);
    setSigningIn(false);
    setTestRun((n) => n + 1);
  };
  const keptOn = saved !== null && (testedOn || signingIn);

  const plan = (): DraftPlan | null => draftPlan(draft, others, botSelection(scope, bots), trust);

  /**
   * Save the server switched off, then write the keys it names. Creating first means a name
   * another server already has is refused before any key is written. A server an earlier test
   * saved is replaced (its name is locked, so it is the same server), which also clears what the
   * other way of connecting left behind; one that was removed from the list meanwhile is fine.
   */
  const persist = async (p: DraftPlan) => {
    if (!client) throw new Error("daemon not connected");
    if (saved) {
      await client.deleteMcpServer(saved).catch((e: unknown) => {
        if (!(e instanceof ApiError && e.status === 404)) throw e;
      });
      setSaved(null);
    }
    await client.createMcpServer(p.config);
    setSaved(p.config.name);
    for (const s of p.secrets) {
      await client.setSecret(s.name, s.value);
    }
  };

  const showErrors = () => {
    setSubmitted(true);
    requestAnimationFrame(() => formRef.current?.querySelector<HTMLElement>("[aria-invalid='true']")?.focus());
  };

  const test = async (): Promise<ConnectionReportLike> => {
    const p = plan();
    if (!client || !p) {
      showErrors();
      throw new Error(t("apps.mcpForm.fixFirst"));
    }
    setError(null);
    setBusy(true);
    setTestedOn(false);
    setSigningIn(false);
    const name = p.config.name;
    // Whatever happens below, a server that did not pass is switched off again; the one exception
    // is a browser sign-in it asked for, which needs the server on to finish.
    let keepOn = false;
    let enabled = false;
    try {
      await persist(p);
      await client.updateMcpServer(name, { enabled: true });
      enabled = true;
      const report = await client.testConnection("mcp", { name });
      const ok = report.outcome === "ok";
      const signIn = !ok && p.signIn && report.outcome === "bad_key";
      keepOn = ok || signIn;
      setTestedOn(ok);
      setSigningIn(signIn);
      if (signIn) onSignIn(name);
      return report;
    } finally {
      if (enabled && !keepOn) {
        await client
          .updateMcpServer(name, { enabled: false })
          .catch(() => setError(t("apps.mcpForm.offFailed", { name })));
      }
      setBusy(false);
    }
  };

  const submit = async () => {
    const p = plan();
    if (!p) {
      showErrors();
      return;
    }
    setError(null);
    setBusy(true);
    try {
      if (!keptOn) {
        await persist(p);
      }
      await onDone(p.config.name, keptOn);
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  const cancel = async () => {
    if (saved && client) {
      await client.deleteMcpServer(saved).catch(() => undefined);
    }
    onCancel();
  };

  const field = (
    name: DraftField,
    label: string,
    input: ReactNode,
    hint?: string,
    warning?: string
  ) => {
    const err = shown(name);
    return (
      <div className={formStyles.field}>
        <label className={formStyles.label} htmlFor={`${id}-${name}`}>{label}</label>
        {input}
        {err ? (
          <p className={formStyles.error} id={`${id}-${name}-err`} role="alert" data-testid={`mcp-err-${name}`}>
            {t(`apps.mcpForm.err.${err}`)}
          </p>
        ) : warning ? (
          <p className={styles.warning} data-testid={`mcp-warn-${name}`}>
            <TriangleAlert aria-hidden="true" />
            {warning}
          </p>
        ) : hint ? (
          <p className={formStyles.hint}>{hint}</p>
        ) : null}
      </div>
    );
  };
  const inputProps = (name: DraftField) => ({
    id: `${id}-${name}`,
    className: formStyles.input,
    spellCheck: false,
    autoComplete: "off",
    "aria-invalid": shown(name) ? true : undefined,
    "aria-describedby": shown(name) ? `${id}-${name}-err` : undefined,
    "data-testid": `mcp-${name}`,
    onBlur: () => touch(name),
    disabled: busy
  });

  const authOption = (mode: McpAuthMode, icon: ReactNode) => (
    <label className={`${styles.choice} ${draft.auth === mode ? styles.choiceOn : ""}`} data-testid={`mcp-auth-${mode}`}>
      <input
        type="radio"
        name={`${id}-auth`}
        checked={draft.auth === mode}
        disabled={busy}
        onChange={() => change({ auth: mode })}
      />
      {icon}
      <span className={styles.choiceText}>
        <b>{t(`apps.mcpForm.auth.${mode}`)}</b>
        <small>{t(`apps.mcpForm.auth.${mode}Hint`)}</small>
      </span>
    </label>
  );

  const nameField = field(
    "name",
    t("apps.mcpForm.name"),
    <input
      {...inputProps("name")}
      value={draft.name}
      readOnly={saved !== null}
      placeholder={draft.mode === "http" ? "my-server" : "my-tool"}
      onChange={(e) => {
        setNameEdited(true);
        change({ name: e.target.value });
      }}
    />,
    saved ? t("apps.mcpForm.nameLocked") : t("apps.mcpForm.nameHint")
  );

  const stdio = draft.mode === "stdio";

  return (
    <form
      ref={formRef}
      className={styles.form}
      noValidate
      data-testid="mcp-custom-form"
      aria-labelledby={`${id}-title`}
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <div className={styles.head}>
        <h3 className={styles.title} id={`${id}-title`}>{t("apps.mcpForm.title")}</h3>
        <p className={formStyles.hint}>{t("apps.mcpForm.intro")}</p>
      </div>

      {stdio ? (
        <section className={styles.section} aria-labelledby={`${id}-cmd-h`}>
          <h4 className={styles.sectionTitle} id={`${id}-cmd-h`}>
            <TerminalSquare aria-hidden="true" />
            {t("apps.mcpForm.commandSection")}
          </h4>
          {field(
            "command",
            t("apps.custom.command"),
            <input
              {...inputProps("command")}
              value={draft.command}
              placeholder="npx -y @scope/server"
              onChange={(e) => change({ command: e.target.value })}
            />,
            t("apps.mcpForm.commandHint")
          )}
          {nameField}
          {field(
            "env",
            t("apps.mcpForm.env"),
            <textarea
              {...inputProps("env")}
              className={formStyles.textarea}
              value={draft.env}
              rows={3}
              placeholder={"LOG_LEVEL=info\nAPI_KEY=…"}
              onChange={(e) => change({ env: e.target.value })}
            />,
            t("apps.mcpForm.envHint")
          )}
        </section>
      ) : (
        <>
          <section className={styles.section} aria-labelledby={`${id}-url-h`}>
            <h4 className={styles.sectionTitle} id={`${id}-url-h`}>
              <Globe aria-hidden="true" />
              {t("apps.mcpForm.urlSection")}
            </h4>
            {field(
              "url",
              t("apps.mcpForm.url"),
              <input
                {...inputProps("url")}
                type="url"
                inputMode="url"
                autoFocus
                value={draft.url}
                placeholder="https://example.com/mcp"
                onChange={(e) => change({ url: e.target.value })}
              />,
              t("apps.mcpForm.urlHint"),
              check.warnings.url ? t(`apps.mcpForm.warn.${check.warnings.url}`) : undefined
            )}
            {nameField}
          </section>
          <section className={styles.section} aria-labelledby={`${id}-auth-h`}>
            <h4 className={styles.sectionTitle} id={`${id}-auth-h`}>
              <KeyRound aria-hidden="true" />
              {t("apps.mcpForm.authSection")}
            </h4>
            <div className={styles.choices} role="radiogroup" aria-labelledby={`${id}-auth-h`}>
              {authOption("none", <Globe className={styles.choiceIcon} aria-hidden="true" />)}
              {authOption("header", <KeyRound className={styles.choiceIcon} aria-hidden="true" />)}
              {authOption("oauth", <LogIn className={styles.choiceIcon} aria-hidden="true" />)}
            </div>
            {draft.auth === "header" ? (
              <div className={styles.pair}>
                {field(
                  "headerName",
                  t("apps.mcpForm.headerName"),
                  <input
                    {...inputProps("headerName")}
                    value={draft.headerName}
                    onChange={(e) => change({ headerName: e.target.value })}
                  />
                )}
                {field(
                  "headerValue",
                  t("apps.mcpForm.headerValue"),
                  <input
                    {...inputProps("headerValue")}
                    type="password"
                    value={draft.headerValue}
                    placeholder="Bearer …"
                    onChange={(e) => change({ headerValue: e.target.value })}
                  />,
                  t("apps.mcpForm.headerHint")
                )}
              </div>
            ) : null}
          </section>
        </>
      )}

      <button
        type="button"
        className={styles.disclosure}
        aria-expanded={stdio}
        data-testid="mcp-stdio-toggle"
        disabled={busy}
        onClick={() => change({ mode: stdio ? "http" : "stdio" })}
      >
        <ChevronDown className={stdio ? styles.chevronOpen : undefined} aria-hidden="true" />
        {stdio ? t("apps.mcpForm.useUrl") : t("apps.mcpForm.useCommand")}
      </button>

      <section className={styles.section} aria-labelledby={`${id}-access-h`}>
        <h4 className={styles.sectionTitle} id={`${id}-access-h`}>{t("apps.access.label")}</h4>
        <BotAccessPicker
          scope={scope}
          bots={bots}
          onChange={(next) => {
            setScope(next);
            invalidate();
          }}
        />
        <McpTrustChoice
          id="custom"
          value={trust}
          disabled={busy}
          compact
          onChange={(next) => {
            setTrust(next);
            invalidate();
          }}
        />
      </section>

      <div className={styles.footer}>
        <ConnectionTestButton
          key={testRun}
          testId="mcp-test"
          label={t("apps.mcpForm.test")}
          disabled={busy || !client || !valid}
          run={test}
        />
        <p className={formStyles.hint} data-testid="mcp-off-note">
          {signingIn
            ? t("apps.mcpForm.signInNote")
            : testedOn
              ? t("apps.mcpForm.onNote")
              : valid
                ? t("apps.mcpForm.offNote")
                : t("apps.mcpForm.testNeeds")}
        </p>
        {error ? <p className={formStyles.error} role="alert">{error}</p> : null}
        <div className={styles.buttons}>
          <Button type="submit" variant="primary" size="sm" disabled={busy || !client} data-testid="mcp-add">
            {keptOn ? t("apps.mcpForm.done") : t("apps.custom.add")}
          </Button>
          <Button variant="ghost" size="sm" disabled={busy} onClick={() => void cancel()}>
            {t("common.cancel")}
          </Button>
        </div>
      </div>
    </form>
  );
}
