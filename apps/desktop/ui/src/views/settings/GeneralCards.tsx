import type { TFunction } from "i18next";
import { Button } from "ui";
import formStyles from "../../components/Form.module.css";
import {
  Segmented,
  SettingRow,
  Switch
} from "../../components/SettingsUi";
import {
  DEFAULT_STOP_HOTKEY,
  HOTKEY_PRESETS
} from "../../config/product";
import { MESSAGE_TIMES_VALUES } from "../../lib/messageTime";
import { SEND_WITH_VALUES } from "../../lib/settingsPrefs";
import { NotificationSettings } from "../NotificationSettings";
import styles from "../SettingsModal.module.css";
import { DictationCard } from "./DictationCard";
import { VoiceSettings } from "./VoiceSettings";
import type { CardDef, SettingsBridge } from "./types";

/** Cards of the "General" section. */
export function generalCards(b: SettingsBridge, t: TFunction): CardDef[] {
  const { settings, update } = b;
  const card = (
    id: string,
    title: string,
    description: string,
    node: React.ReactNode,
    over: Partial<CardDef> = {}
  ): CardDef => ({
    id,
    section: "general",
    title,
    description,
    kind: "row",
    node,
    ...over
  });

  return [
    card(
      "account",
      t("settingsModal.general.account.title"),
      t("settingsModal.general.account.description"),
      <SettingRow
        title={t("settingsModal.general.account.title")}
        description={t("settingsModal.general.account.description")}
      >
        <input
          className={`${formStyles.input} ${styles.shortInput}`}
          value={settings.accountName}
          maxLength={40}
          aria-label={t("settingsModal.general.account.title")}
          placeholder={t("account.defaultName")}
          onChange={(e) => update({ accountName: e.target.value })}
        />
      </SettingRow>
    ),
    card(
      "language",
      t("settings.language.title"),
      t("settings.language.description"),
      <SettingRow
        title={t("settings.language.title")}
        description={t("settings.language.description")}
      >
        <Segmented
          label={t("settings.language.title")}
          value={settings.language}
          onChange={b.onLanguage}
          options={[
            { value: "en", label: t("settings.language.en") },
            { value: "tr", label: t("settings.language.tr") },
            { value: "de", label: t("settings.language.de") }
          ]}
        />
      </SettingRow>,
      { keywords: "dil turkce english" }
    ),
    card(
      "send",
      t("settingsModal.general.send.title"),
      t("settingsModal.general.send.description"),
      <SettingRow
        title={t("settingsModal.general.send.title")}
        description={t("settingsModal.general.send.description")}
      >
        <Segmented
          label={t("settingsModal.general.send.title")}
          value={settings.sendWith}
          onChange={(sendWith) => update({ sendWith })}
          options={SEND_WITH_VALUES.map((v) => ({
            value: v,
            label: t(`settingsModal.general.send.${v}`)
          }))}
        />
      </SettingRow>,
      { keywords: "enter ctrl gonder send" }
    ),
    card(
      "times",
      t("settingsModal.general.times.title"),
      t("settingsModal.general.times.description"),
      <SettingRow
        title={t("settingsModal.general.times.title")}
        description={t("settingsModal.general.times.description")}
      >
        <Segmented
          label={t("settingsModal.general.times.title")}
          value={settings.messageTimes}
          onChange={(messageTimes) => update({ messageTimes })}
          options={MESSAGE_TIMES_VALUES.map((v) => ({
            value: v,
            label: t(`settingsModal.general.times.${v}`)
          }))}
        />
      </SettingRow>,
      { keywords: "saat zaman time clock seconds saniye mesaj" }
    ),
    card(
      "speak-replies",
      t("voice.speakAuto.title"),
      t("voice.speakAuto.description"),
      <SettingRow
        title={t("voice.speakAuto.title")}
        description={t("voice.speakAuto.description")}
      >
        <Switch
          label={t("voice.speakAuto.title")}
          checked={settings.speakReplies}
          onChange={(speakReplies) => update({ speakReplies })}
        />
      </SettingRow>,
      { keywords: "ses voice speak read aloud tts sesli oku vorlesen" }
    ),
    card(
      "voice",
      t("voice.title"),
      t("voice.description"),
      <VoiceSettings settings={settings} update={update} />,
      { keywords: "ses voice speech tts sesli oku vorlesen stimme" }
    ),
    card(
      "dictation",
      t("voice.stt.title"),
      t("voice.stt.description"),
      <DictationCard />,
      { keywords: "dictation mikrofon microphone whisper transcribe yazdir diktat" }
    ),
    card(
      "autostart",
      t("settings.autostart.title"),
      t("settings.autostart.description"),
      <SettingRow
        title={t("settings.autostart.title")}
        description={t("settings.autostart.description")}
      >
        <Switch
          label={t("settings.autostart.title")}
          checked={settings.autostart}
          onChange={() => b.onToggleAutostart()}
        />
        {b.autostartError ? (
          <span className={formStyles.error} role="alert">{b.autostartError}</span>
        ) : null}
      </SettingRow>,
      { keywords: "startup baslangic" }
    ),
    card(
      "keepInTray",
      t("settings.keepInTray.title"),
      t("settings.keepInTray.description"),
      <SettingRow
        title={t("settings.keepInTray.title")}
        description={t("settings.keepInTray.description")}
      >
        <Switch
          label={t("settings.keepInTray.title")}
          checked={settings.keepInTray}
          onChange={() => update({ keepInTray: !settings.keepInTray })}
        />
      </SettingRow>,
      { keywords: "tray taskbar system tray tepsi bildirim alani hintergrund infobereich minimize" }
    ),
    card(
      "hotkey",
      t("settings.hotkey.title"),
      t("settings.hotkey.description"),
      <SettingRow
        stacked
        title={t("settings.hotkey.title")}
        description={t("settings.hotkey.description")}
      >
        <span className={styles.chip}>
          {settings.stopHotkey || DEFAULT_STOP_HOTKEY}
        </span>
        <Button
          size="sm"
          variant={b.recordingHotkey ? "primary" : "secondary"}
          onClick={b.onToggleRecordHotkey}
        >
          {b.recordingHotkey
            ? t("settings.hotkey.recording")
            : t("settings.hotkey.record")}
        </Button>
        {HOTKEY_PRESETS.map((p) => (
          <Button
            key={p.id}
            size="sm"
            variant="secondary"
            active={settings.stopHotkey.toLowerCase() === p.value.toLowerCase()}
            onClick={() => b.onStopHotkey(p.value)}
          >
            {t(`settings.hotkey.presets.${p.id}`)}
          </Button>
        ))}
      </SettingRow>,
      { keywords: "shortcut kisayol stop" }
    ),
    card(
      "notifications",
      t("settings.notifications.title"),
      t("settings.notifications.description"),
      <NotificationSettings settings={settings} onChange={update} />,
      { kind: "block", keywords: "bildirim notify" }
    ),
    card(
      "onboarding",
      t("settings.onboarding.title"),
      t("settings.onboarding.description"),
      <SettingRow
        title={t("settings.onboarding.title")}
        description={t("settings.onboarding.description")}
      >
        <Button
          size="sm"
          variant="secondary"
          data-testid="settings-open-onboarding"
          onClick={() => b.onOpenOnboarding(settings.onboardingCompleted)}
        >
          {settings.onboardingCompleted
            ? t("settings.onboarding.restart")
            : t("settings.onboarding.resume")}
        </Button>
      </SettingRow>,
      { keywords: "setup wizard kurulum" }
    )
  ];
}
