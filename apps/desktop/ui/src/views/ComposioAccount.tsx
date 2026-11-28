import { type ReactNode, type Ref, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ComposioStatus } from "../api/types";
import { AppLogo } from "../components/AppLogo";
import formStyles from "../components/Form.module.css";
import { openComposioLink } from "../lib/externalLink";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import type { LoginPhase } from "../state/useComposio";
import styles from "./AppsModal.module.css";
import own from "./AppsGrid.module.css";

/**
 * The Composio account as an entry of the Apps list: its logo, the account it is signed in as,
 * and Sign in / Sign out. A key form or the sign-out question opens under the row.
 */
export function ComposioAccount({
  rowRef,
  children,
  status,
  login,
  startLogin,
  cancelLogin,
  disabled,
  onChanged
}: {
  rowRef?: Ref<HTMLLIElement>;
  /** Shown under the row (a status error with its retry). */
  children?: ReactNode;
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
  const failed = login === "failed" && !linked;
  const open = confirm || (keyOpen && !linked && status !== null) || failed || error !== null;
  return (
    <li
      ref={rowRef}
      tabIndex={-1}
      className={`${styles.cell} ${open ? styles.cellOpen : ""}`}
      aria-labelledby="composio-account-heading"
      data-testid="composio-account"
      data-state={linked ? "linked" : signingIn ? "waiting" : "signed_out"}
    >
      <div className={styles.cellRow}>
        <AppLogo name="Composio" size={26} />
        <div className={styles.cellText}>
          <b className={styles.cellName}>
            <span className={styles.cellTitle} id="composio-account-heading">{t("apps.composio.advanced.title")}</span>
          </b>
          <small className={`${styles.cellSub} ${linked ? styles.tone_ok : ""}`} role="status">
            {linked
              ? status.account ? t("apps.connectedAs", { account: status.account }) : t("apps.composioHub.accountConnected")
              : signingIn ? t("apps.composio.link.waiting")
                : status?.login_expired ? t("apps.composio.catalogError.unauthorized")
                  : t("apps.composioRow.hint")}
          </small>
        </div>
        {linked ? (
          <button type="button" className={styles.connect} data-testid="composio-sign-out"
            disabled={disabled || busy || signingIn} onClick={() => setConfirm(true)}>
            {t("apps.composioRow.signOut")}
          </button>
        ) : signingIn ? (
          <>
            <span className={`${styles.state} ${styles.tone_busy}`}>
              <span className={styles.dot} aria-hidden="true" />
              {t("apps.composio.waiting")}
            </span>
            <button type="button" className={styles.connect} onClick={cancelLogin}>{t("common.cancel")}</button>
          </>
        ) : (
          <>
            {status ? (
              <button type="button" className={styles.manage + " " + styles.manageShown} aria-expanded={keyOpen}
                data-testid="composio-use-key" disabled={disabled || busy}
                onClick={() => { setError(null); setKeyOpen((o) => !o); }}>
                {t("apps.composioRow.useKey")}
              </button>
            ) : null}
            <button type="button" className={styles.connect} data-testid="composio-sign-in"
              disabled={disabled || busy || !client || !status}
              onClick={() => { setKeyOpen(false); void startLogin(); }}>
              {t(status?.login_expired ? "apps.composioRow.signInAgain" : "apps.composioRow.signIn")}
            </button>
          </>
        )}
      </div>
      {open ? (
        <div className={styles.panel}>
          {failed ? <p className={formStyles.error} role="alert">{t("apps.composioHub.loginFailed")}</p> : null}
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
        </div>
      ) : null}
      {children}
    </li>
  );
}
