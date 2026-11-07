import type { CSSProperties } from "react";
import { useRef } from "react";
import type { TFunction } from "i18next";
import { Button } from "ui";
import formStyles from "../../components/Form.module.css";
import {
  Segmented,
  SettingRow,
  Switch
} from "../../components/SettingsUi";
import {
  ACCENT_PRESETS,
  BLUR_PRESETS,
  DEFAULT_STOP_HOTKEY,
  DIM_PRESETS,
  HOTKEY_PRESETS
} from "../../config/product";
import { SEND_WITH_VALUES } from "../../lib/settingsPrefs";
import { NotificationSettings } from "../NotificationSettings";
import styles from "../SettingsModal.module.css";
import type { CardDef, SettingsBridge } from "./types";

function BackgroundControls({ b, t }: { b: SettingsBridge; t: TFunction }) {
  const fileRef = useRef<HTMLInputElement | null>(null);
  const { settings, update } = b;
  const onFile = (file: File | undefined) => {
    if (!file) {
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      const result = typeof reader.result === "string" ? reader.result : "";
      if (result) {
        update({ backgroundUrl: result, backgroundFileName: file.name });
      }
    };
    reader.readAsDataURL(file);
  };
  return (
    <div className={styles.stack}>
      <input
        ref={fileRef}
        type="file"
        className={styles.hidden}
        accept="image/png,image/jpeg,image/webp,image/gif,image/svg+xml"
        onChange={(e) => {
          onFile(e.target.files?.[0]);
          e.target.value = "";
        }}
      />
      <div className={styles.inline}>
        <Button size="sm" variant="secondary" onClick={() => fileRef.current?.click()}>
          {t("settings.background.chooseFile")}
        </Button>
        {settings.backgroundUrl ? (
          <Button
            size="sm"
            variant="ghost"
            onClick={() => update({ backgroundUrl: "", backgroundFileName: "" })}
          >
            {t("settings.background.clearFile")}
          </Button>
        ) : null}
        <span className={formStyles.hint}>
          {settings.backgroundFileName || t("settings.background.noFile")}
        </span>
      </div>
      <div className={styles.inline}>
        <span className={formStyles.hint}>{t("settings.background.blur")}</span>
        {BLUR_PRESETS.map((p) => (
          <Button
            key={p.id}
            size="sm"
            variant="secondary"
            active={settings.backgroundBlur === p.value}
            onClick={() => update({ backgroundBlur: p.value })}
          >
            {t(`settings.background.blurLevels.${p.id}`)}
          </Button>
        ))}
      </div>
      <div className={styles.inline}>
        <span className={formStyles.hint}>{t("settings.background.dim")}</span>
        {DIM_PRESETS.map((p) => (
          <Button
            key={p.id}
            size="sm"
            variant="secondary"
            active={settings.backgroundDim === p.value}
            onClick={() => update({ backgroundDim: p.value })}
          >
            {t(`settings.background.dimLevels.${p.id}`)}
          </Button>
        ))}
      </div>
    </div>
  );
}

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
      "theme",
      t("settingsModal.general.theme.title"),
      t("settingsModal.general.theme.description"),
      <SettingRow
        title={t("settingsModal.general.theme.title")}
        description={t("settingsModal.general.theme.description")}
      >
        <Segmented
          label={t("settingsModal.general.theme.title")}
          value={settings.themeMode}
          onChange={b.onTheme}
          options={[
            { value: "system", label: t("settingsModal.general.theme.system") },
            { value: "light", label: t("settings.theme.light") },
            { value: "dark", label: t("settings.theme.dark") }
          ]}
        />
      </SettingRow>,
      { keywords: "dark light mode tema karanlik aydinlik" }
    ),
    card(
      "accent",
      t("settings.accent.title"),
      t("settings.accent.description"),
      <SettingRow
        title={t("settings.accent.title")}
        description={t("settings.accent.description")}
      >
        <div className={styles.swatches}>
          {ACCENT_PRESETS.map((p) => {
            const selected =
              settings.accentColor.toLowerCase() === p.value.toLowerCase();
            return (
              <button
                key={p.id}
                type="button"
                className={[styles.swatch, selected ? styles.swatchActive : ""]
                  .filter(Boolean)
                  .join(" ")}
                style={{ "--swatch-color": p.value } as CSSProperties}
                title={t(`settings.accent.presets.${p.id}`)}
                aria-label={t(`settings.accent.presets.${p.id}`)}
                aria-pressed={selected}
                onClick={() => b.onAccent(p.value)}
              />
            );
          })}
          <label className={styles.customColor}>
            <input
              type="color"
              className={styles.colorInput}
              aria-label={t("settings.accent.custom")}
              value={settings.accentColor || "#2563eb"}
              onChange={(e) => b.onAccent(e.target.value)}
            />
          </label>
          {settings.accentColor ? (
            <Button size="sm" variant="ghost" onClick={() => b.onAccent("")}>
              {t("settings.accent.reset")}
            </Button>
          ) : null}
        </div>
      </SettingRow>,
      { keywords: "color renk vurgu" }
    ),
    card(
      "frame",
      t("settingsModal.general.frame.title"),
      t("settingsModal.general.frame.description"),
      <SettingRow
        title={t("settingsModal.general.frame.title")}
        description={t("settingsModal.general.frame.description")}
      >
        <Switch
          label={t("settingsModal.general.frame.title")}
          checked={settings.windowFrame}
          onChange={b.onWindowFrame}
        />
      </SettingRow>,
      { keywords: "window title bar pencere cerceve" }
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
            { value: "tr", label: t("settings.language.tr") }
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
          <span className={formStyles.error}>{b.autostartError}</span>
        ) : null}
      </SettingRow>,
      { keywords: "startup baslangic" }
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
      "background",
      t("settings.background.title"),
      t("settings.background.description"),
      <SettingRow
        stacked
        title={t("settings.background.title")}
        description={t("settings.background.description")}
      >
        <BackgroundControls b={b} t={t} />
      </SettingRow>,
      { keywords: "wallpaper image arka plan" }
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
