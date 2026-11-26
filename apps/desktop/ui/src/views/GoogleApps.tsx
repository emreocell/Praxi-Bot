import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { McpCatalogEntry, McpServer } from "../api/types";
import { BrandIcon } from "../components/BrandIcon";
import formStyles from "../components/Form.module.css";
import { openSignInLink } from "../lib/externalLink";
import {
  GOOGLE_STEPS,
  type GoogleFieldError,
  checkGoogleClient,
  googleClientIdOf,
  googleScopes,
  googleServerOf,
  googleServices
} from "../lib/googleSetup";
import styles from "./AppsModal.module.css";
import own from "./GoogleApps.module.css";

/**
 * Gmail, Drive and Calendar through Google's own MCP servers, with the owner's own Google client.
 * One card per app; the first Connect opens the guided setup (exact console pages, then the client
 * id and secret), the next apps reuse the same client. Read-only access only.
 */
export function GoogleApps({
  entries,
  servers,
  busy,
  onConnect
}: {
  entries: McpCatalogEntry[];
  servers: McpServer[];
  busy: string | null;
  /** Save the client when one was typed, add the app and sign in. */
  onConnect: (entry: McpCatalogEntry, typed: { clientId: string; clientSecret: string } | null) => Promise<void>;
}) {
  const { t } = useTranslation();
  const known = useMemo(() => googleClientIdOf(servers), [servers]);
  const [setupFor, setSetupFor] = useState<McpCatalogEntry | null>(null);
  const waiting = entries.filter((e) => !googleServerOf(e, servers));
  return (
    <section className={styles.section} aria-labelledby="apps-google-h" data-testid="google-apps">
      <h3 className={styles.sectionLabel} id="apps-google-h">
        {t("apps.google.title")}
      </h3>
      <p className={own.intro}>{t("apps.google.intro")}</p>
      {waiting.length === 0 ? <p className={own.intro}>{t("apps.google.allAdded")}</p> : null}
      <ul className={styles.grid}>
        {waiting.map((entry) => (
          <li key={entry.id} className={styles.cell} data-testid={`google-app-${entry.id}`}>
            <div className={styles.cellRow}>
              <BrandIcon name={entry.icon ?? entry.name} bare size={26} />
              <div className={styles.cellText}>
                <b className={styles.cellName}>
                  <span className={styles.cellTitle}>{entry.name}</span>
                </b>
                <small className={styles.cellSub}>{t("apps.google.readOnly")}</small>
              </div>
              <Button
                size="sm"
                variant="secondary"
                disabled={busy !== null}
                data-testid={`google-connect-${entry.id}`}
                onClick={() => (known ? void onConnect(entry, null) : setSetupFor(entry))}
              >
                {t("apps.connect")}
              </Button>
            </div>
          </li>
        ))}
      </ul>
      {known && !setupFor ? (
        <p className={own.intro}>
          <Button size="sm" variant="ghost" data-testid="google-change-client" onClick={() => setSetupFor(waiting[0] ?? entries[0] ?? null)}>
            {t("apps.google.changeClient")}
          </Button>
        </p>
      ) : null}
      {setupFor ? (
        <GoogleSetup
          entries={entries}
          first={setupFor}
          busy={busy !== null}
          onCancel={() => setSetupFor(null)}
          onSave={async (typed) => {
            const entry = setupFor;
            setSetupFor(null);
            await onConnect(entry, typed);
          }}
        />
      ) : null}
      <details className={own.trouble}>
        <summary>{t("apps.google.trouble.title")}</summary>
        <ul>
          <li>{t("apps.google.trouble.unverified")}</li>
          <li>{t("apps.google.trouble.sevenDays")}</li>
          <li>{t("apps.google.trouble.workspace")}</li>
          <li>{t("apps.google.trouble.secret")}</li>
        </ul>
      </details>
    </section>
  );
}

/** The guided setup: the exact pages of the Google Cloud console, then the client id and secret. */
function GoogleSetup({
  entries,
  first,
  busy,
  onCancel,
  onSave
}: {
  entries: McpCatalogEntry[];
  first: McpCatalogEntry;
  busy: boolean;
  onCancel: () => void;
  onSave: (typed: { clientId: string; clientSecret: string }) => Promise<void>;
}) {
  const { t } = useTranslation();
  const [picked, setPicked] = useState<string[]>([first.id]);
  const [clientId, setClientId] = useState("");
  const [clientSecret, setClientSecret] = useState("");
  const [bad, setBad] = useState<GoogleFieldError | null>(null);
  const [copied, setCopied] = useState(false);
  const chosen = entries.filter((e) => picked.includes(e.id));
  const scopes = googleScopes(chosen);
  const services = googleServices(chosen);
  return (
    <div className={own.setup} data-testid="google-setup">
      <ol className={own.steps}>
        {GOOGLE_STEPS.map((step) => (
          <li key={step.id} className={own.step} data-testid={`google-step-${step.id}`}>
            <b>{t(`apps.google.steps.${step.id}.title`)}</b>
            <p>{t(`apps.google.steps.${step.id}.body`)}</p>
            {step.id === "apis" ? (
              <>
                <div className={own.picks}>
                  {entries.map((e) => (
                    <label key={e.id}>
                      <input
                        type="checkbox"
                        checked={picked.includes(e.id)}
                        onChange={(ev) =>
                          setPicked((p) => (ev.target.checked ? [...p, e.id] : p.filter((id) => id !== e.id)))
                        }
                      />{" "}
                      {e.name}
                    </label>
                  ))}
                </div>
                <div className={own.links}>
                  {services.map((s) => (
                    <Button key={s.id} size="sm" variant="secondary" onClick={() => void openSignInLink(s.href)}>
                      {s.label}
                    </Button>
                  ))}
                </div>
              </>
            ) : (
              <div className={own.links}>
                {step.links.map((l) => (
                  <Button key={l.href} size="sm" variant="secondary" onClick={() => void openSignInLink(l.href)}>
                    {l.label}
                  </Button>
                ))}
              </div>
            )}
            {step.id === "scopes" ? (
              <>
                <div className={own.scopes}>
                  {scopes.map((s) => (
                    <code key={s}>{s}</code>
                  ))}
                </div>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => {
                    void navigator.clipboard?.writeText(scopes.join("\n")).then(
                      () => setCopied(true),
                      () => setCopied(false)
                    );
                  }}
                >
                  {copied ? t("apps.google.copied") : t("apps.google.copyScopes")}
                </Button>
              </>
            ) : null}
          </li>
        ))}
      </ol>
      <form
        className={own.fields}
        onSubmit={(e) => {
          e.preventDefault();
          const problem = checkGoogleClient(clientId, clientSecret);
          setBad(problem);
          if (!problem) {
            void onSave({ clientId: clientId.trim(), clientSecret: clientSecret.trim() });
          }
        }}
      >
        <label>
          <span className={formStyles.hint}>{t("apps.google.clientId")}</span>
          <input
            className={formStyles.input}
            value={clientId}
            onChange={(e) => setClientId(e.target.value)}
            placeholder="123456789012-….apps.googleusercontent.com"
            autoComplete="off"
            spellCheck={false}
            data-testid="google-client-id"
          />
        </label>
        <label>
          <span className={formStyles.hint}>{t("apps.google.clientSecret")}</span>
          <input
            className={formStyles.input}
            type="password"
            value={clientSecret}
            onChange={(e) => setClientSecret(e.target.value)}
            autoComplete="off"
            spellCheck={false}
            data-testid="google-client-secret"
          />
        </label>
        <p className={formStyles.hint}>{t("apps.google.secretNote")}</p>
        {bad ? (
          <p className={own.bad} role="alert">
            {t(`apps.google.error.${bad}`)}
          </p>
        ) : null}
        <div className={own.actions}>
          <Button size="sm" variant="ghost" type="button" onClick={onCancel}>
            {t("common.cancel")}
          </Button>
          <Button size="sm" variant="primary" type="submit" disabled={busy} data-testid="google-save">
            {t("apps.google.save")}
          </Button>
        </div>
      </form>
    </div>
  );
}
