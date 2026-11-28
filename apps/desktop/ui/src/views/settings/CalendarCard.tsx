import { CalendarPlus, CircleAlert, CircleCheck, PlugZap, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import type { CalendarAccount, CalendarFound, CalendarTestReport } from "../../api/types";
import { MoreInfo } from "../../components/ChoiceList";
import { FormField, formStyles } from "../../components/SettingsForm";
import { SettingGroup, SettingsDisclosure } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import { humanLine } from "../../lib/humanError";
import {
  CALENDAR_PRESETS,
  type CalendarPresetId,
  calendarPresetUrl,
  isValidCalendarName,
  suggestCalendarName
} from "../../lib/calendarPresets";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./CalendarCard.module.css";

/** Calendar accounts the bot can read and change: add, test and remove (CalDAV + app password). */
export function CalendarCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();

  const [accounts, setAccounts] = useState<CalendarAccount[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [advanced, setAdvanced] = useState(false);

  const [preset, setPreset] = useState<CalendarPresetId>("icloud");
  const [server, setServer] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [url, setUrl] = useState("");
  const [urlEdited, setUrlEdited] = useState(false);
  const [name, setName] = useState("");
  const [nameEdited, setNameEdited] = useState(false);
  const [email, setEmail] = useState("");
  const [report, setReport] = useState<CalendarTestReport | null>(null);

  const existing = (accounts ?? []).map((a) => a.name);
  const shownName = nameEdited ? name : suggestCalendarName(preset, existing);
  const shownUrl = urlEdited ? url : calendarPresetUrl(preset, { username, server });
  const needsServer = CALENDAR_PRESETS.find((p) => p.id === preset)?.needsServer ?? false;
  const isOther = preset === "other";

  const refresh = useCallback(async () => {
    if (!client) {
      return;
    }
    try {
      setAccounts(await client.listCalendarAccounts());
      setLoadError(null);
    } catch (e) {
      setLoadError(humanLine(e));
    }
  }, [client]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const forgetResult = () => setReport(null);

  const choosePreset = (id: CalendarPresetId) => {
    setPreset(id);
    setUrlEdited(false);
    setAdvanced(id === "other");
    forgetResult();
  };

  const runTest = async () => {
    if (!client) {
      return;
    }
    setBusy(true);
    try {
      setReport(await client.testCalendar({ url: shownUrl, username: username.trim(), password }));
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
    }
  };

  const runSavedTest = async (account: CalendarAccount) => {
    if (!client) {
      return;
    }
    setBusy(true);
    try {
      const res = await client.testCalendar({ name: account.name });
      toast.show(
        res.ok
          ? t("settingsModal.calendar.savedTestOk", { name: account.name, count: res.calendars.length })
          : t("settingsModal.calendar.testFailed", { error: res.error ?? "" }),
        res.ok ? "success" : "danger"
      );
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
    }
  };

  const pickCalendar = (found: CalendarFound) => {
    setUrl(found.url);
    setUrlEdited(true);
  };

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!client) {
      return;
    }
    setBusy(true);
    try {
      await client.createCalendarAccount({
        name: shownName,
        url: shownUrl,
        username: username.trim(),
        password,
        email: email.trim() || null
      });
      toast.show(t("settingsModal.calendar.toast.added", { name: shownName }), "success");
      setPassword("");
      setUsername("");
      setEmail("");
      setName("");
      setNameEdited(false);
      setUrl("");
      setUrlEdited(false);
      forgetResult();
      await refresh();
    } catch (err) {
      toast.show(humanLine(err), "danger");
    } finally {
      setBusy(false);
    }
  };

  const remove = async (account: CalendarAccount) => {
    if (!client) {
      return;
    }
    setBusy(true);
    try {
      await client.deleteCalendarAccount(account.name);
      toast.show(t("settingsModal.calendar.toast.removed", { name: account.name }), "success");
      await refresh();
    } catch (err) {
      toast.show(humanLine(err), "danger");
    } finally {
      setBusy(false);
    }
  };

  const ready =
    shownUrl.trim() !== "" && username.trim() !== "" && password !== "" && !busy && client !== null;
  const canSave = ready && isValidCalendarName(shownName);
  const presetHint = t(`settingsModal.calendar.presets.${preset}.hintShort`, {
    defaultValue: t(`settingsModal.calendar.presets.${preset}.hint`)
  });

  return (
    <div className={styles.root} data-testid="calendar-card">
      <MoreInfo testId="calendar-more">
        <p>{t("settingsModal.calendar.note")}</p>
      </MoreInfo>

      {loadError ? (
        <p className={formStyles.error} role="alert">
          {t("settingsModal.calendar.loadError", { message: loadError })}
        </p>
      ) : null}

      {accounts && accounts.length > 0 ? (
        <SettingGroup>
          <ul className={styles.list}>
            {accounts.map((a) => (
              <li key={a.name} className={styles.row} data-testid={`calendar-account-${a.name}`}>
                <div className={styles.rowText}>
                  <span className={styles.name}>{a.name}</span>
                  <span className={styles.detail}>{a.username}</span>
                  <span className={styles.detail}>{a.url}</span>
                </div>
                <div className={styles.rowControls}>
                  <Button size="sm" variant="secondary" disabled={busy} onClick={() => void runSavedTest(a)}>
                    <PlugZap />
                    {t("settingsModal.calendar.test")}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    aria-label={t("settingsModal.calendar.remove", { name: a.name })}
                    title={t("settingsModal.calendar.remove", { name: a.name })}
                    onClick={() => void remove(a)}
                  >
                    <Trash2 />
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        </SettingGroup>
      ) : accounts ? (
        <p className={formStyles.hint}>{t("settingsModal.calendar.empty")}</p>
      ) : null}

      <Card asChild padding="md" elevated>
        <form className={styles.card} onSubmit={(e) => void save(e)}>
          <h4 className={styles.subheading}>{t("settingsModal.calendar.addTitle")}</h4>

          <FormField
            id="calendar-preset"
            label={t("settingsModal.calendar.serviceLabel")}
            hint={presetHint}
            helpTitle={t(`settingsModal.calendar.presets.${preset}.label`)}
            helpBody={t(`settingsModal.calendar.presets.${preset}.hint`)}
            width="md"
          >
            <select
              id="calendar-preset"
              className={formStyles.select}
              value={preset}
              onChange={(e) => choosePreset(e.target.value as CalendarPresetId)}
            >
              {CALENDAR_PRESETS.map((p) => (
                <option key={p.id} value={p.id}>
                  {t(`settingsModal.calendar.presets.${p.id}.label`)}
                </option>
              ))}
            </select>
          </FormField>

          {needsServer ? (
            <FormField
              id="calendar-server"
              label={t("settingsModal.calendar.serverLabel")}
              hint={t("settingsModal.calendar.serverShort")}
              helpTitle={t("settingsModal.calendar.serverLabel")}
              helpBody={t("settingsModal.calendar.presets.nextcloud.hint")}
              width="md"
            >
              <input
                id="calendar-server"
                type="text"
                className={formStyles.input}
                value={server}
                placeholder="cloud.example.com"
                autoComplete="off"
                onChange={(e) => {
                  setServer(e.target.value);
                  forgetResult();
                }}
              />
            </FormField>
          ) : null}

          <div className={styles.pair}>
            <FormField
              id="calendar-username"
              label={t("settingsModal.calendar.usernameLabel")}
              width="md"
            >
              <input
                id="calendar-username"
                type="text"
                className={formStyles.input}
                value={username}
                autoComplete="off"
                onChange={(e) => {
                  setUsername(e.target.value);
                  forgetResult();
                }}
              />
            </FormField>
            <FormField
              id="calendar-password"
              label={t("settingsModal.calendar.passwordLabel")}
              hint={t("settingsModal.calendar.passwordShort")}
              helpTitle={t("settingsModal.calendar.passwordLabel")}
              helpBody={t("settingsModal.calendar.passwordHint")}
              width="md"
            >
              <input
                id="calendar-password"
                type="password"
                className={formStyles.input}
                value={password}
                autoComplete="new-password"
                onChange={(e) => {
                  setPassword(e.target.value);
                  forgetResult();
                }}
              />
            </FormField>
          </div>

          {isOther ? (
            <FormField
              id="calendar-url"
              label={t("settingsModal.calendar.urlLabel")}
              hint={t("settingsModal.calendar.urlShort")}
              helpTitle={t("settingsModal.calendar.urlLabel")}
              helpBody={t("settingsModal.calendar.urlHint")}
            >
              <input
                id="calendar-url"
                type="url"
                className={formStyles.input}
                value={shownUrl}
                autoComplete="off"
                onChange={(e) => {
                  setUrl(e.target.value);
                  setUrlEdited(true);
                  forgetResult();
                }}
              />
            </FormField>
          ) : null}
          <SettingsDisclosure
            open={advanced}
            count={isOther ? 2 : 3}
            label={t("settingsModal.calendar.advanced")}
            hint={t("settingsModal.calendar.advancedHint")}
            testId="calendar-advanced"
            onToggle={() => setAdvanced((o) => !o)}
          >
            {!isOther ? (
              <FormField
                id="calendar-url"
                label={t("settingsModal.calendar.urlLabel")}
                hint={t("settingsModal.calendar.urlShort")}
                helpTitle={t("settingsModal.calendar.urlLabel")}
                helpBody={t("settingsModal.calendar.urlHint")}
              >
                <input
                  id="calendar-url"
                  type="url"
                  className={formStyles.input}
                  value={shownUrl}
                  autoComplete="off"
                  onChange={(e) => {
                    setUrl(e.target.value);
                    setUrlEdited(true);
                    forgetResult();
                  }}
                />
              </FormField>
            ) : null}
            <div className={styles.pair}>
              <FormField
                id="calendar-name"
                label={t("settingsModal.calendar.nameLabel")}
                hint={t("settingsModal.calendar.nameShort")}
                helpTitle={t("settingsModal.calendar.nameLabel")}
                helpBody={t("settingsModal.calendar.nameHint")}
                width="sm"
              >
                <input
                  id="calendar-name"
                  type="text"
                  className={formStyles.input}
                  value={shownName}
                  autoComplete="off"
                  onChange={(e) => {
                    setName(e.target.value);
                    setNameEdited(true);
                  }}
                />
              </FormField>
              <FormField
                id="calendar-email"
                label={t("settingsModal.calendar.emailLabel")}
                hint={t("settingsModal.calendar.emailShort")}
                helpTitle={t("settingsModal.calendar.emailLabel")}
                helpBody={t("settingsModal.calendar.emailHint")}
                width="md"
              >
                <input
                  id="calendar-email"
                  type="email"
                  className={formStyles.input}
                  value={email}
                  autoComplete="off"
                  onChange={(e) => setEmail(e.target.value)}
                />
              </FormField>
            </div>
          </SettingsDisclosure>

          <div className={styles.actions}>
            <Button
              type="button"
              size="sm"
              variant="secondary"
              disabled={!ready}
              data-testid="calendar-test-btn"
              onClick={() => void runTest()}
            >
              <PlugZap />
              {busy ? t("settingsModal.calendar.testing") : t("settingsModal.calendar.test")}
            </Button>
            <Button type="submit" size="sm" variant="primary" disabled={!canSave} data-testid="calendar-add-btn">
              <CalendarPlus />
              {t("settingsModal.calendar.add")}
            </Button>
          </div>

          {report ? (
            <div className={styles.card} data-testid="calendar-test-result">
              <div className={`${styles.pill} ${report.ok ? styles.pillOk : styles.pillBad}`}>
                {report.ok ? <CircleCheck aria-hidden="true" /> : <CircleAlert aria-hidden="true" />}
                <span>
                  {report.ok
                    ? t("settingsModal.calendar.testOk", { count: report.calendars.length })
                    : t("settingsModal.calendar.testFailed", { error: report.error ?? "" })}
                </span>
              </div>
              {report.ok ? (
                <p className={formStyles.hint}>
                  {report.scheduling
                    ? t("settingsModal.calendar.schedulingOn")
                    : t("settingsModal.calendar.schedulingOff")}
                </p>
              ) : null}
              {report.ok && report.calendars.length > 0 ? (
                <>
                  <p className={formStyles.hint}>{t("settingsModal.calendar.pickHint")}</p>
                  <ul className={styles.found}>
                    {report.calendars.map((c) => (
                      <li key={c.url}>
                        <label className={styles.foundLabel}>
                          <input
                            type="radio"
                            name="calendar-found"
                            checked={shownUrl === c.url}
                            onChange={() => pickCalendar(c)}
                          />
                          <span className={styles.rowText}>
                            <span className={styles.name}>{c.name}</span>
                            <span className={styles.detail}>{c.url}</span>
                          </span>
                        </label>
                      </li>
                    ))}
                  </ul>
                </>
              ) : null}
            </div>
          ) : null}
        </form>
      </Card>
    </div>
  );
}