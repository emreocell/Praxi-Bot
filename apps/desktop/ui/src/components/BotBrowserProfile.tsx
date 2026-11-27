import { LogIn, RefreshCw, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { BrowserProfileStatus } from "../api/types";
import { humanError, type HumanError } from "../lib/humanError";
import { formatBytes } from "../lib/workspaceFormat";
import { useDaemon } from "../state/DaemonContext";
import { ConfirmDialog } from "./DialogParts";
import { ErrorNotice } from "./ErrorNotice";
import styles from "./BotBrowserProfile.module.css";

/** Owner-only housekeeping; credentials are typed in the native browser, never this form. */
export function BotBrowserProfile({ botId }: { botId: string }) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [profile, setProfile] = useState<BrowserProfileStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<HumanError | null>(null);
  const [forget, setForget] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (!client) return;
    try {
      setProfile(await client.browserProfile(botId));
      setError(null);
    } catch (e) {
      setError(humanError(e));
    }
  }, [client, botId]);

  useEffect(() => {
    void load();
    // Closing the native browser by its X button must release manual mode too.
    const onFocus = () => void load();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [load]);

  const act = async (action: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      await action();
      setForget(null);
      await load();
    } catch (e) {
      setError(humanError(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className={styles.root} aria-labelledby="browser-profile-title" data-testid="browser-profile">
      <h3 id="browser-profile-title" className={styles.label}>{t("browserProfile.title")}</h3>
      <p>{t("browserProfile.intro")}</p>
      <p className={styles.warning}>{t("browserProfile.warning")}</p>
      <div className={styles.actions}>
        <Button disabled={!client || busy || !profile} onClick={() => {
          if (!client) return;
          void act(() => profile?.signing_in ? client.finishBrowserSignIn(botId) : client.startBrowserSignIn(botId));
        }}>
          <LogIn aria-hidden="true" />
          {t(profile?.signing_in ? "browserProfile.done" : "browserProfile.signIn")}
        </Button>
        <Button variant="ghost" disabled={!client || busy} onClick={() => void load()} aria-label={t("browserProfile.refresh")}>
          <RefreshCw aria-hidden="true" />{t("browserProfile.refresh")}
        </Button>
      </div>
      {profile?.signing_in ? <p role="status">{t("browserProfile.manualHint")}</p> : null}
      {error && forget === null ? <ErrorNotice error={error} compact /> : null}
      {profile ? <>
        <h4 className={styles.label}>{t("browserProfile.sites")}</h4>
        <p>{t("browserProfile.siteHint")}</p>
        {!profile.sites_available ? <p role="status">{t("browserProfile.sitesUnavailable")}</p> : profile.domains.length ? <ul className={styles.sites}>
          {profile.domains.map((domain) => <li key={domain}>
            <span>{domain}</span>
            <Button variant="ghost" disabled={busy || profile.signing_in} onClick={() => { setError(null); setForget(domain); }}>
              <Trash2 aria-hidden="true" />{t("browserProfile.forgetNamed", { domain })}
            </Button>
          </li>)}
        </ul> : <p>{t("browserProfile.empty")}</p>}
        {/* The full facts about the profile, folded: the tab opens on what to do, not on a wall of text. */}
        <details className={styles.about}>
          <summary>{t("browserProfile.about")}</summary>
          <p>{t("browserProfile.isolation")}</p>
          <p>{t(profile.directory_encrypted ? "browserProfile.encrypted" : "browserProfile.protection")}</p>
          <p>{t("browserProfile.sizeHuman", { size: formatBytes(profile.size_bytes) })}</p>
        </details>
        {/* Destructive, but behind a danger confirmation: a quiet button, not a red bar. */}
        <Button variant="secondary" size="sm" className={styles.forgetAll} disabled={!client || busy || profile.signing_in || profile.size_bytes === 0}
          onClick={() => { setError(null); setForget(""); }}>
          <Trash2 aria-hidden="true" />{t("browserProfile.forgetAll")}
        </Button>
      </> : null}
      <ConfirmDialog open={forget !== null} tone="danger" busy={busy}
        title={t(forget === "" ? "browserProfile.forgetAll" : "browserProfile.forgetSite")}
        body={<>
          <p>{forget === "" ? t("browserProfile.confirmAll") : t("browserProfile.confirmSite", { domain: forget })}</p>
          {error ? <ErrorNotice error={error} compact /> : null}
        </>}
        confirmLabel={t(forget === "" ? "browserProfile.forgetAll" : "browserProfile.forgetSite")}
        onCancel={() => { setForget(null); setError(null); }}
        onConfirm={() => {
          if (!client || forget === null) return;
          void act(() => forget === "" ? client.forgetBrowserProfile(botId) : client.forgetBrowserSite(botId, forget));
        }} />
    </section>
  );
}
