import { Globe, LogIn, RefreshCw, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { BrowserProfileStatus } from "../api/types";
import { humanError, type HumanError } from "../lib/humanError";
import { formatBytes } from "../lib/workspaceFormat";
import { useDaemon } from "../state/DaemonContext";
import { ConfirmDialog } from "./DialogParts";
import { ErrorNotice } from "./ErrorNotice";
import { PanelGroup, PanelRows, StatusChip, type StatusTone } from "./PanelRows";
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

  const signingIn = profile?.signing_in === true;
  const siteCount = profile?.domains.length ?? 0;
  const status: { tone: StatusTone; label: string } | null = !profile
    ? null
    : signingIn
      ? { tone: "warn", label: t("panelTabs.browser.status.signingIn") }
      : !profile.sites_available
        ? { tone: "muted", label: t("panelTabs.browser.status.unavailable") }
        : siteCount === 0
          ? { tone: "muted", label: t("browserProfile.empty") }
          : { tone: "ok", label: t("panelTabs.browser.status.sites", { count: siteCount }) };

  return (
    <PanelGroup title={t("browserProfile.title")} testId="browser-profile">
      <div className={styles.card}>
        <div className={styles.head}>
          {status ? <StatusChip tone={status.tone} testId="browser-profile-status">{status.label}</StatusChip> : <span />}
          <Button size="sm" variant="primary" disabled={!client || busy || !profile} onClick={() => {
            if (!client) return;
            void act(() => signingIn ? client.finishBrowserSignIn(botId) : client.startBrowserSignIn(botId));
          }}>
            <LogIn aria-hidden="true" />
            {t(signingIn ? "browserProfile.done" : "browserProfile.signIn")}
          </Button>
        </div>
        <p className={styles.caution}>{t("panelTabs.browser.caution")}</p>
        {signingIn ? <p className={styles.note} role="status">{t("browserProfile.manualHint")}</p> : null}
        {error && forget === null ? <ErrorNotice error={error} compact /> : null}
        {profile ? <>
          {profile.sites_available && siteCount > 0 ? <PanelRows
            testId="browser-profile-sites"
            label={t("browserProfile.sites")}
            searchLabel={t("panelTabs.browser.searchSites")}
            limit={4}
            searchFrom={6}
            rows={profile.domains.map((domain) => ({
              id: domain,
              name: domain,
              icon: Globe,
              action: {
                label: t("browserProfile.forgetNamed", { domain }),
                icon: Trash2,
                disabled: busy || signingIn,
                onClick: () => { setError(null); setForget(domain); }
              }
            }))}
          /> : null}
          {/* The full facts about the profile, folded: the tab opens on what to do, not on a wall of text. */}
          <details className={styles.about}>
            <summary>{t("browserProfile.about")}</summary>
            <p>{t("browserProfile.intro")}</p>
            <p>{t("browserProfile.warning")}</p>
            <p>{profile.sites_available ? t("browserProfile.siteHint") : t("browserProfile.sitesUnavailable")}</p>
            <p>{t("browserProfile.isolation")}</p>
            <p>{t(profile.directory_encrypted ? "browserProfile.encrypted" : "browserProfile.protection")}</p>
            <p>{t("browserProfile.sizeHuman", { size: formatBytes(profile.size_bytes) })}</p>
            <div className={styles.aboutActions}>
              <Button variant="ghost" size="sm" disabled={!client || busy} onClick={() => void load()}>
                <RefreshCw aria-hidden="true" />{t("browserProfile.refresh")}
              </Button>
              {/* Destructive, but behind a danger confirmation: a quiet button, not a red bar. */}
              <Button variant="secondary" size="sm" disabled={!client || busy || signingIn || profile.size_bytes === 0}
                onClick={() => { setError(null); setForget(""); }}>
                <Trash2 aria-hidden="true" />{t("browserProfile.forgetAll")}
              </Button>
            </div>
          </details>
        </> : null}
      </div>
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
    </PanelGroup>
  );
}
