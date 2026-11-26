import { CalendarPlus, CircleAlert, CircleCheck, Info, KeyRound, PlugZap, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card } from "ui";
import type { CalendarAccount, CalendarFound, CalendarTestReport } from "../../api/types";
import formStyles from "../../components/Form.module.css";
import { SettingGroup } from "../../components/SettingsUi";
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

  return (
    <div className={styles.root} data-testid="calendar-card">
      <p className={styles.note}>
        <Info aria-hidden="true" />
        <span>{t("settingsModal.calendar.note")}</span>
      </p>

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

          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="calendar-preset">
              {t("settingsModal.calendar.serviceLabel")}
            </label>
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
            <p className={formStyles.hint}>{t(`settingsModal.calendar.presets.${preset}.hint`)}</p>
          </div>

          {needsServer ? (
            <div className={formStyles.field}>
              <label className={formStyles.label} htmlFor="calendar-server">
                {t("settingsModal.calendar.serverLabel")}
              </label>
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
            </div>
          ) : null}

          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="calendar-username">
              {t("settingsModal.calendar.usernameLabel")}
            </label>
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
          </div>

          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="calendar-password">
              {t("settingsModal.calendar.passwordLabel")}
            </label>
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
            <p className={formStyles.hint}>
              <KeyRound aria-hidden="true" /> {t("settingsModal.calendar.passwordHint")}
            </p>
          </div>

          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="calendar-url">
              {t("settingsModal.calendar.urlLabel")}
            </label>
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
            <p className={formStyles.hint}>{t("settingsModal.calendar.urlHint")}</p>
          </div>

          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="calendar-name">
              {t("settingsModal.calendar.nameLabel")}
            </label>
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
            <p className={formStyles.hint}>{t("settingsModal.calendar.nameHint")}</p>
          </div>

          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor="calendar-email">
              {t("settingsModal.calendar.emailLabel")}
            </label>
            <input
              id="calendar-email"
              type="email"
              className={formStyles.input}
              value={email}
              autoComplete="off"
              onChange={(e) => setEmail(e.target.value)}
            />
            <p className={formStyles.hint}>{t("settingsModal.calendar.emailHint")}</p>
          </div>

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
