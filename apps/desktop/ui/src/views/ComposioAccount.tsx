import { useState } from "react";
import { Plug } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ComposioStatus } from "../api/types";
import formStyles from "../components/Form.module.css";
import { openComposioLink } from "../lib/externalLink";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import type { LoginPhase } from "../state/useComposio";
import styles from "./AppsModal.module.css";
import own from "./AppsGrid.module.css";

/** One Composio account for the entire toolkit catalog. */
export function ComposioAccount({
  status,
  login,
  startLogin,
  cancelLogin,
  disabled,
  onChanged
}: {
  status: ComposioStatus | null;
  login: LoginPhase;
  startLogin: () => Promise<void>;
  cancelLogin: () => void;
  disabled: boolean;
  onChanged: () => Promise<void>;
}) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [keyOpen, setKeyOpen] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const linked = status?.configured === true && !status.login_expired;
  const signingIn = login === "starting" || login === "waiting";
  const run = (fn: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    void fn()
      .then(async () => {
        setValue("");
        setKeyOpen(false);
        setConfirm(false);
        await onChanged();
      })
      .catch((e: unknown) => setError(humanLine(e)))
      .finally(() => setBusy(false));
  };
  return (
    <section className={own.accountCard} aria-labelledby="composio-account-heading" data-testid="composio-account">
      <div className={own.accountRow}>
        <span className={own.accountIcon}><Plug aria-hidden="true" /></span>
        <div className={own.accountCopy}>
          <h2 className={own.accountTitle} id="composio-account-heading">{t("apps.composio.advanced.title")}</h2>
          <p className={own.quiet} role="status">
            {linked
              ? status.account ? t("apps.connectedAs", { account: status.account }) : t("apps.composioHub.accountConnected")
              : signingIn ? t("apps.composio.link.waiting")
                : status?.login_expired ? t("apps.composio.catalogError.unauthorized")
                  : t("apps.composioHub.accountHint")}
          </p>
        </div>
        <div className={styles.actions}>
          {linked ? (
            <Button size="sm" variant="ghost" disabled={disabled || busy || signingIn} onClick={() => setConfirm(true)}>
              {t("apps.composio.advanced.signOut")}
            </Button>
          ) : signingIn ? (
            <Button size="sm" variant="ghost" onClick={cancelLogin}>{t("common.cancel")}</Button>
          ) : (
            <Button size="sm" variant="primary" disabled={disabled || busy || !client || !status}
              onClick={() => { setKeyOpen(false); void startLogin(); }}>
              {t(status?.login_expired ? "apps.composio.relogin" : "apps.composioHub.linkAccount")}
            </Button>
          )}
          {!linked && !signingIn && status ? (
            <Button size="sm" variant="secondary" disabled={disabled || busy} aria-expanded={keyOpen}
              onClick={() => { setError(null); setKeyOpen((o) => !o); }}>
              {t("apps.composioHub.useKey")}
            </Button>
          ) : null}
        </div>
      </div>
      {login === "failed" && !linked ? <p className={formStyles.error} role="alert">{t("apps.composioHub.loginFailed")}</p> : null}
      {confirm ? (
        <div className={styles.actions}>
          <p className={own.quiet}>{t("apps.composioHub.signOutHint")}</p>
          <Button size="sm" variant="danger" disabled={disabled || busy || !client}
            onClick={() => { if (client) run(() => client.removeComposioKey()); }}>
            {t("apps.composio.advanced.signOut")}
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setConfirm(false)}>{t("common.cancel")}</Button>
        </div>
      ) : null}
      {keyOpen && !linked && status ? (
        <form
          className={own.keyForm}
          onSubmit={(e) => {
            e.preventDefault();
            if (client && value.trim()) run(() => client.setComposioKey(value.trim()));
          }}
        >
          <button type="button" className={styles.link} onClick={() => void openComposioLink(status.key_setup_url, true)}>
            {t("apps.composio.getKey")}
          </button>
          <label className={formStyles.label} htmlFor="composio-key">
            {t("apps.composio.keyLabel")}
          </label>
          <input
            id="composio-key"
            className={formStyles.input}
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={value}
            onChange={(e) => setValue(e.target.value)}
          />
          <p className={formStyles.hint}>{t("apps.composio.keyHint")}</p>
          <Button type="submit" size="sm" variant="primary" disabled={disabled || busy || !client || value.trim() === ""}>
            {t("apps.composio.keySave")}
          </Button>
        </form>
      ) : null}
      {error ? <p className={formStyles.error} role="alert">{error}</p> : null}
    </section>
  );
}
