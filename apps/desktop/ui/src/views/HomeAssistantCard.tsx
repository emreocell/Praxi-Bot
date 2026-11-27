import { ChevronDown, ChevronRight, CircleAlert, CircleCheck, KeyRound, PlugZap, ShieldCheck } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { ApiError } from "../api/client";
import type { HomeAssistantSettings } from "../api/types";
import { AppLogo } from "../components/AppLogo";
import { BotAccessPicker } from "../components/BotAccessPicker";
import formStyles from "../components/Form.module.css";
import { useToast } from "../components/Toast";
import { canSaveHomeAssistant, formatServiceList, parseServiceList } from "../lib/homeAssistant";
import { humanLine } from "../lib/humanError";
import { type BotScope, botScope, botSelection, botsFromScope } from "../lib/mcpApps";
import { useDaemon } from "../state/DaemonContext";
import styles from "./HomeAssistantCard.module.css";

/** The daemon words this one for the owner ("Home Assistant refused the access token ..."): show it as is. */
function errorLine(e: unknown): string {
  return e instanceof ApiError && e.code === "bad_homeassistant_config" ? e.message : humanLine(e);
}

type Report = { ok: true; entities: number } | { ok: false; error: string };

/**
 * Home Assistant, set up from the app: address and long-lived token (kept in the secure store, never
 * shown again), "Save and test", which bots may use it, the services bots may call without a card,
 * and Remove. Owner-only on the daemon; no file is edited by hand.
 */
export function HomeAssistantCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, bots } = useDaemon();

  const [settings, setSettings] = useState<HomeAssistantSettings | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(false);
  const [address, setAddress] = useState<string | null>(null);
  const [token, setToken] = useState("");
  const [servicesText, setServicesText] = useState<string | null>(null);
  const [report, setReport] = useState<Report | null>(null);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [selection, setSelection] = useState<BotScope | null>(null);

  const refresh = useCallback(async () => {
    if (!client) return;
    try {
      setSettings(await client.getHomeAssistant());
      setLoadError(null);
    } catch (e) {
      setLoadError(humanLine(e));
    }
  }, [client]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const configured = settings?.configured === true;
  const shownAddress = address ?? settings?.url ?? "";
  const shownServices = servicesText ?? formatServiceList(settings?.trusted_services ?? []);
  const parsed = parseServiceList(shownServices);
  const canSave = !busy && client !== null && canSaveHomeAssistant(settings, shownAddress, token, parsed.invalid);
  const scope = selection ?? botSelection(botScope({ bots: settings?.bots ?? null }), bots);

  const forgetResult = () => setReport(null);

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!client) return;
    setBusy(true);
    setReport(null);
    try {
      const saved = await client.saveHomeAssistant({
        url: shownAddress.trim(),
        token: token.trim() === "" ? null : token.trim(),
        trusted_services: parsed.services
      });
      setSettings(saved.settings);
      setToken("");
      setAddress(null);
      setServicesText(null);
      setReport({ ok: true, entities: saved.entities });
      toast.show(t("apps.homeAssistant.toast.saved", { count: saved.entities }), "success");
    } catch (err) {
      setReport({ ok: false, error: errorLine(err) });
    } finally {
      setBusy(false);
    }
  };

  const runTest = async () => {
    if (!client) return;
    setBusy(true);
    try {
      // A typed token means the form is being tried before it is saved; otherwise the saved hub.
      const typed = token.trim() !== "";
      const res = await client.testHomeAssistant(typed ? { url: shownAddress.trim(), token: token.trim() } : {});
      setReport(res.ok ? { ok: true, entities: res.entities ?? 0 } : { ok: false, error: res.error ?? "" });
    } catch (err) {
      setReport({ ok: false, error: errorLine(err) });
    } finally {
      setBusy(false);
    }
  };

  const saveAccess = async () => {
    if (!client) return;
    setBusy(true);
    try {
      setSettings(await client.setHomeAssistantBots(botsFromScope(scope)));
      setSelection(null);
      toast.show(t("apps.homeAssistant.access.saved"), "success");
    } catch (err) {
      toast.show(errorLine(err), "danger");
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!client) return;
    setBusy(true);
    try {
      await client.removeHomeAssistant();
      setConfirmRemove(false);
      setReport(null);
      setToken("");
      setAddress(null);
      setServicesText(null);
      setSelection(null);
      toast.show(t("apps.homeAssistant.toast.removed"), "success");
      await refresh();
    } catch (err) {
      toast.show(errorLine(err), "danger");
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className={styles.root} aria-labelledby="ha-title" data-testid="ha-card" data-state={configured ? "on" : "off"}>
      <button
        type="button"
        className={styles.head}
        aria-expanded={open}
        aria-controls="ha-body"
        data-testid="ha-toggle"
        onClick={() => setOpen((o) => !o)}
      >
        <AppLogo name="homeassistant" size={26} />
        <span className={styles.headText}>
          <span className={styles.title} id="ha-title">{t("apps.homeAssistant.title")}</span>
          <span className={styles.sub}>
            {configured ? (settings?.url ?? "") : t("apps.homeAssistant.description")}
          </span>
        </span>
        <span className={`${styles.state} ${configured ? styles.stateOn : ""}`} role="status">
          {configured ? <CircleCheck aria-hidden="true" /> : null}
          {configured ? t("apps.homeAssistant.status.on") : t("apps.homeAssistant.status.off")}
        </span>
        {open ? <ChevronDown aria-hidden="true" /> : <ChevronRight aria-hidden="true" />}
      </button>

      {open ? (
        <div className={styles.body} id="ha-body">
          {loadError ? (
            <p className={formStyles.error} role="alert">
              {t("apps.homeAssistant.loadError", { message: loadError })}
            </p>
          ) : null}

          <p className={styles.note}>
            <ShieldCheck aria-hidden="true" />
            <span>{t("apps.homeAssistant.riskNote")}</span>
          </p>

          <form className={styles.form} onSubmit={(e) => void save(e)}>
            <div className={formStyles.field}>
              <label className={formStyles.label} htmlFor="ha-address">{t("apps.homeAssistant.addressLabel")}</label>
              <input
                id="ha-address"
                type="url"
                className={formStyles.input}
                value={shownAddress}
                placeholder="http://homeassistant.local:8123"
                autoComplete="off"
                spellCheck={false}
                onChange={(e) => {
                  setAddress(e.target.value);
                  forgetResult();
                }}
              />
              <p className={formStyles.hint}>{t("apps.homeAssistant.addressHint")}</p>
            </div>

            <div className={formStyles.field}>
              <label className={formStyles.label} htmlFor="ha-token">{t("apps.homeAssistant.tokenLabel")}</label>
              <input
                id="ha-token"
                type="password"
                className={formStyles.input}
                value={token}
                placeholder={settings?.token_set ? t("apps.homeAssistant.tokenStored") : ""}
                autoComplete="new-password"
                onChange={(e) => {
                  setToken(e.target.value);
                  forgetResult();
                }}
              />
              <p className={formStyles.hint}>
                <KeyRound aria-hidden="true" /> {t("apps.homeAssistant.tokenHint")}
              </p>
            </div>

            <div className={formStyles.field}>
              <label className={formStyles.label} htmlFor="ha-services">{t("apps.homeAssistant.servicesLabel")}</label>
              <textarea
                id="ha-services"
                className={`${formStyles.input} ${formStyles.textarea}`}
                value={shownServices}
                placeholder={t("apps.homeAssistant.servicesPlaceholder")}
                rows={3}
                autoComplete="off"
                spellCheck={false}
                aria-invalid={parsed.invalid.length > 0}
                onChange={(e) => {
                  setServicesText(e.target.value);
                  forgetResult();
                }}
              />
              {parsed.invalid.length > 0 ? (
                <p className={formStyles.error} role="alert">
                  {t("apps.homeAssistant.servicesInvalid", { names: parsed.invalid.join(", ") })}
                </p>
              ) : null}
              <p className={formStyles.hint}>{t("apps.homeAssistant.servicesHint")}</p>
            </div>

            <div className={styles.actions}>
              <Button type="submit" size="sm" variant="primary" disabled={!canSave} data-testid="ha-save">
                {busy ? t("apps.homeAssistant.checking") : t("apps.homeAssistant.saveAndTest")}
              </Button>
              {configured || token.trim() !== "" ? (
                <Button
                  type="button"
                  size="sm"
                  variant="secondary"
                  disabled={busy || client === null || (token.trim() !== "" && shownAddress.trim() === "")}
                  data-testid="ha-test"
                  onClick={() => void runTest()}
                >
                  <PlugZap />
                  {t("apps.homeAssistant.test")}
                </Button>
              ) : null}
              {configured && !confirmRemove ? (
                <Button type="button" size="sm" variant="ghost" disabled={busy} data-testid="ha-remove" onClick={() => setConfirmRemove(true)}>
                  {t("apps.homeAssistant.remove")}
                </Button>
              ) : null}
            </div>
          </form>

          {confirmRemove ? (
            <div className={styles.confirm} role="alert">
              <p className={formStyles.hint}>{t("apps.homeAssistant.removeHint")}</p>
              <div className={styles.actions}>
                <Button size="sm" variant="danger" disabled={busy} data-testid="ha-remove-confirm" onClick={() => void remove()}>
                  {t("apps.homeAssistant.removeConfirm")}
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setConfirmRemove(false)}>{t("common.cancel")}</Button>
              </div>
            </div>
          ) : null}

          {report ? (
            <div
              className={`${styles.pill} ${report.ok ? styles.pillOk : styles.pillBad}`}
              role={report.ok ? "status" : "alert"}
              data-testid="ha-report"
            >
              {report.ok ? <CircleCheck aria-hidden="true" /> : <CircleAlert aria-hidden="true" />}
              <span>
                {report.ok
                  ? t("apps.homeAssistant.testOk", { count: report.entities })
                  : t("apps.homeAssistant.testFailed", { error: report.error })}
              </span>
            </div>
          ) : null}

          {configured ? (
            <section className={styles.access} aria-labelledby="ha-access-title">
              <h3 className={styles.sectionTitle} id="ha-access-title">{t("apps.homeAssistant.access.title")}</h3>
              <BotAccessPicker
                scope={scope}
                bots={bots}
                onChange={setSelection}
                texts={{
                  label: t("apps.homeAssistant.access.title"),
                  all: t("apps.homeAssistant.access.all"),
                  some: t("apps.homeAssistant.access.some"),
                  nobody: t("apps.homeAssistant.access.nobody")
                }}
                allWarning={t("apps.homeAssistant.access.warning")}
              />
              <div className={styles.actions}>
                <Button size="sm" variant="primary" disabled={busy || selection === null} data-testid="ha-access-save" onClick={() => void saveAccess()}>
                  {t("apps.homeAssistant.access.save")}
                </Button>
              </div>
            </section>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}
