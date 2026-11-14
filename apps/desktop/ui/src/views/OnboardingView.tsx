import { ThemePicker } from "../components/ThemePicker";
import type { ChangeEvent, CSSProperties } from "react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button, Card } from "ui";
import type { Message, PermissionPreset } from "../api/types";
import { ApprovalCard, ToolCard } from "../components/ChatCards";
import formStyles from "../components/Form.module.css";
import { DEFAULT_STOP_HOTKEY } from "../config/product";
import type { SupportedLanguage } from "../i18n";
import { avatarFromFile, AvatarFileError } from "../lib/avatar";
import {
  BOT_COLORS,
  configFromDraft,
  draftFromBot
} from "../lib/botProfile";
import {
  addProvider,
  FAKE_PROVIDER_ID,
  setDefaultProvider,
  testProvider
} from "../lib/providers";
import {
  type OnboardingPresetGroup,
  providerRoleDefaults,
  providersView
} from "../lib/providerView";
import { isTauriRuntime } from "../overlay/events";
import { useDaemon } from "../state/DaemonContext";
import type { TimelineItem } from "../state/timeline";
import type { AppSettings } from "../store/settingsStore";
import styles from "./OnboardingView.module.css";

const TOTAL_STEPS = 5;
const CUSTOM_OPENAI_ID = "__custom_openai__";

const SAFETY_PRESETS: PermissionPreset[] = [
  "approve-destructive",
  "read-only",
  "read-and-draft",
  "full"
];

const OPENAI_COMPAT_CATALOG = [
  "openrouter",
  "deepseek",
  "lmstudio",
  "gemini-openai-compat"
] as const;

export interface OnboardingViewProps {
  settings: AppSettings;
  onUpdateSettings: (patch: Partial<AppSettings>) => void;
  onLanguageChange: (lang: SupportedLanguage) => void;
  onSkip: (currentStep: number) => void;
  onComplete: (result: { botId?: string; chatId?: string }) => void;
}

function clampStep(step: number): number {
  if (!Number.isFinite(step) || step < 1) {
    return 1;
  }
  if (step > TOTAL_STEPS) {
    return TOTAL_STEPS;
  }
  return Math.floor(step);
}

function presetGroupForProvider(
  providerId: string,
  fakeAvailable: boolean
): OnboardingPresetGroup {
  if (providerId === "gemini") {
    return "gemini";
  }
  if (providerId === "anthropic") {
    return "anthropic";
  }
  if (providerId === "ollama") {
    return "ollama";
  }
  if (providerId === FAKE_PROVIDER_ID && fakeAvailable) {
    return "fake";
  }
  if (
    OPENAI_COMPAT_CATALOG.includes(
      providerId as (typeof OPENAI_COMPAT_CATALOG)[number]
    )
  ) {
    return "openai_compatible";
  }
  return fakeAvailable && !providerId ? "fake" : "gemini";
}

export function OnboardingView({
  settings,
  onUpdateSettings,
  onLanguageChange,
  onSkip,
  onComplete
}: OnboardingViewProps) {
  const { t } = useTranslation();
  const {
    phase,
    error: daemonError,
    conn,
    client,
    bots,
    events,
    refreshBots,
    refreshChats,
    refreshRuns,
    retry,
    restartDaemon
  } = useDaemon();
  const native = isTauriRuntime();
  const fakeAvailable = Boolean(conn?.fake_provider);

  const [step, setStep] = useState<number>(() =>
    clampStep(settings.onboardingStep || 1)
  );

  // Step 2: Provider state
  const initialProviderId =
    settings.onboardingProviderId ||
    (fakeAvailable ? FAKE_PROVIDER_ID : "gemini");
  const [presetGroup, setPresetGroup] = useState<OnboardingPresetGroup>(() =>
    presetGroupForProvider(initialProviderId, fakeAvailable)
  );
  const [openAiChoice, setOpenAiChoice] = useState<string>(() =>
    OPENAI_COMPAT_CATALOG.includes(
      initialProviderId as (typeof OPENAI_COMPAT_CATALOG)[number]
    )
      ? initialProviderId
      : "openrouter"
  );
  const [customProviderId, setCustomProviderId] = useState("custom-openai");
  const [baseUrl, setBaseUrl] = useState<string>(
    () => providerRoleDefaults(initialProviderId).defaultBaseUrl
  );
  const [apiKey, setApiKey] = useState("");
  const [keyStoredFor, setKeyStoredFor] = useState<Record<string, boolean>>({});
  const [editingKey, setEditingKey] = useState(false);
  const [providerModel, setProviderModel] = useState<string>(
    () =>
      settings.onboardingModel ||
      providerRoleDefaults(initialProviderId).defaultModel
  );
  const [providerBusy, setProviderBusy] = useState(false);
  const [providerSavedMsg, setProviderSavedMsg] = useState<string | null>(null);
  const [testSuccessMsg, setTestSuccessMsg] = useState<string | null>(null);
  const [testErrorMsg, setTestErrorMsg] = useState<string | null>(null);

  // Step 3: Safety preset state
  const [safetyPreset, setSafetyPreset] = useState<PermissionPreset>(
    () => settings.onboardingPermissionPreset || "approve-destructive"
  );
  const [safetyBusy, setSafetyBusy] = useState(false);
  const [safetyError, setSafetyError] = useState<string | null>(null);

  // Step 4: First bot state
  const [botName, setBotName] = useState<string>(() =>
    t("onboarding.step4.defaultBotName")
  );
  const [botColor, setBotColor] = useState<string>(BOT_COLORS[0]);
  const [botAvatar, setBotAvatar] = useState<string | undefined>(undefined);
  const [avatarError, setAvatarError] = useState<string | null>(null);
  const [botPersona, setBotPersona] = useState<string>(() =>
    t("onboarding.step4.defaultPersona")
  );
  const [executorModel, setExecutorModel] = useState("");
  const [plannerModel, setPlannerModel] = useState("");
  const [classifierModel, setClassifierModel] = useState("");
  const [createdBotId, setCreatedBotId] = useState<string>(
    () => settings.onboardingBotId || ""
  );
  const [botBusy, setBotBusy] = useState(false);
  const [botError, setBotError] = useState<string | null>(null);
  const avatarInputRef = useRef<HTMLInputElement | null>(null);

  // Step 5: Live try-it state
  const [tryChatId, setTryChatId] = useState<string>("");
  const [tryMessages, setTryMessages] = useState<Message[]>([]);
  const [customPrompt, setCustomPrompt] = useState("");
  const [lastTriedPrompt, setLastTriedPrompt] = useState<{
    display: string;
    wire: string;
  } | null>(null);
  const [tryBusy, setTryBusy] = useState(false);
  const [tryError, setTryError] = useState<string | null>(null);
  const promptAliasRef = useRef<Record<string, string>>({});

  // Resolve active provider ID from Step 2 selection
  const resolvedProviderId = (() => {
    switch (presetGroup) {
      case "gemini":
        return "gemini";
      case "anthropic":
        return "anthropic";
      case "ollama":
        return "ollama";
      case "fake":
        return FAKE_PROVIDER_ID;
      case "openai_compatible":
        return openAiChoice === CUSTOM_OPENAI_ID
          ? customProviderId.trim() || "custom-openai"
          : openAiChoice;
    }
  })();

  const roleDefaults = providerRoleDefaults(resolvedProviderId, providerModel);

  // Sync existing configured providers from daemon so `keyStoredFor` reflects OS keychain state
  useEffect(() => {
    if (!client) {
      return;
    }
    client
      .providers()
      .then((dto) => {
        const view = providersView(dto);
        const stored: Record<string, boolean> = {};
        for (const p of view.configured) {
          if (p.hasKey) {
            stored[p.id] = true;
          }
        }
        setKeyStoredFor((prev) => ({ ...prev, ...stored }));
      })
      .catch(() => {});
  }, [client]);

  // Pre-fill role models when moving to Step 4 or changing provider/model
  useEffect(() => {
    const defaults = providerRoleDefaults(resolvedProviderId, providerModel);
    setExecutorModel(defaults.executor);
    setPlannerModel(defaults.planner);
    setClassifierModel(defaults.classifier);
  }, [resolvedProviderId, providerModel]);

  // Live runs for the tryChatId from DaemonContext's global event stream
  const tryChatRuns = useMemo(
    () =>
      tryChatId
        ? Object.values(events.runs)
            .filter((r) => r.chatId === tryChatId)
            .sort((a, b) => a.startedMs - b.startedMs)
        : [],
    [events.runs, tryChatId]
  );

  const tryFinishedCount = tryChatRuns.filter(
    (r) => r.finishedMs !== undefined
  ).length;

  useEffect(() => {
    if (!client || !tryChatId) {
      return;
    }
    client
      .listMessages(tryChatId)
      .then(setTryMessages)
      .catch(() => {});
    if (tryChatRuns.length > 0 && tryChatRuns.every((r) => r.finishedMs !== undefined)) {
      setTryBusy(false);
    }
  }, [client, tryChatId, tryFinishedCount, tryChatRuns]);

  const goToStep = (nextStep: number) => {
    const clamped = clampStep(nextStep);
    setStep(clamped);
    onUpdateSettings({
      onboardingStep: clamped,
      onboardingProviderId: resolvedProviderId,
      onboardingModel: providerModel
    });
  };

  const selectPresetGroup = (group: OnboardingPresetGroup) => {
    setPresetGroup(group);
    setProviderSavedMsg(null);
    setTestSuccessMsg(null);
    setTestErrorMsg(null);
    setEditingKey(false);

    const nextId =
      group === "gemini"
        ? "gemini"
        : group === "anthropic"
          ? "anthropic"
          : group === "ollama"
            ? "ollama"
            : group === "fake"
              ? FAKE_PROVIDER_ID
              : openAiChoice === CUSTOM_OPENAI_ID
                ? customProviderId.trim() || "custom-openai"
                : openAiChoice;
    const defaults = providerRoleDefaults(nextId);
    setProviderModel(defaults.defaultModel);
    setBaseUrl(defaults.defaultBaseUrl);
  };

  const handleOpenAiSubChoice = (nextChoice: string) => {
    setOpenAiChoice(nextChoice);
    setProviderSavedMsg(null);
    setTestSuccessMsg(null);
    setTestErrorMsg(null);
    const id =
      nextChoice === CUSTOM_OPENAI_ID
        ? customProviderId.trim() || "custom-openai"
        : nextChoice;
    const defaults = providerRoleDefaults(id);
    setProviderModel(defaults.defaultModel);
    setBaseUrl(defaults.defaultBaseUrl);
  };

  const saveCurrentProvider = async (): Promise<boolean> => {
    if (presetGroup === "fake") {
      onUpdateSettings({
        onboardingProviderId: FAKE_PROVIDER_ID,
        onboardingModel: providerModel.trim() || "echo"
      });
      setProviderSavedMsg(
        t("onboarding.step2.savedOk", { id: FAKE_PROVIDER_ID })
      );
      return true;
    }

    if (!native) {
      setTestErrorMsg(t("onboarding.step2.nativeWarning"));
      return false;
    }

    setProviderBusy(true);
    setTestErrorMsg(null);
    try {
      const isCustom =
        presetGroup === "openai_compatible" &&
        openAiChoice === CUSTOM_OPENAI_ID;
      const customUrl =
        isCustom || presetGroup === "ollama" ? baseUrl.trim() : undefined;
      const res = await addProvider({
        id: resolvedProviderId,
        baseUrl: customUrl || undefined,
        model: providerModel.trim() || roleDefaults.defaultModel,
        apiKey: apiKey.trim() || undefined
      });
      if (res.code !== 0) {
        setTestErrorMsg((res.stderr || res.stdout).trim());
        return false;
      }

      // Immediately clear the API key from component state so it is never shown again
      if (apiKey.trim() !== "") {
        setApiKey("");
        setKeyStoredFor((prev) => ({ ...prev, [resolvedProviderId]: true }));
        setEditingKey(false);
      }

      await setDefaultProvider(
        resolvedProviderId,
        providerModel.trim() || roleDefaults.defaultModel
      ).catch(() => {});

      try {
        await client?.reloadProviders();
      } catch {
        if (conn?.started_by_app) {
          await restartDaemon().catch(() => {});
        }
      }

      onUpdateSettings({
        onboardingProviderId: resolvedProviderId,
        onboardingModel: providerModel.trim() || roleDefaults.defaultModel
      });
      setProviderSavedMsg(
        t("onboarding.step2.savedOk", { id: resolvedProviderId })
      );
      return true;
    } catch (e) {
      setTestErrorMsg(e instanceof Error ? e.message : String(e));
      return false;
    } finally {
      setProviderBusy(false);
    }
  };

  const handleTestProvider = async () => {
    setTestSuccessMsg(null);
    setTestErrorMsg(null);

    if (presetGroup === "fake") {
      setProviderBusy(true);
      try {
        if (!client) {
          throw new Error(daemonError || t("onboarding.daemonBanner.offlineTitle"));
        }
        const h = await client.health();
        setTestSuccessMsg(
          t("onboarding.step2.testOk", {
            detail: `${FAKE_PROVIDER_ID} (${h.status}, v${h.version})`
          })
        );
        onUpdateSettings({
          onboardingProviderId: FAKE_PROVIDER_ID,
          onboardingModel: providerModel.trim() || "echo"
        });
      } catch (e) {
        setTestErrorMsg(e instanceof Error ? e.message : String(e));
      } finally {
        setProviderBusy(false);
      }
      return;
    }

    if (!native) {
      setTestErrorMsg(t("onboarding.step2.nativeWarning"));
      return;
    }

    const saved = await saveCurrentProvider();
    if (!saved) {
      return;
    }

    setProviderBusy(true);
    try {
      const res = await testProvider(resolvedProviderId);
      const detail = (res.code === 0 ? res.stdout : res.stderr || res.stdout).trim();
      if (res.code === 0) {
        setTestSuccessMsg(t("onboarding.step2.testOk", { detail }));
        setTestErrorMsg(null);
      } else {
        setTestErrorMsg(detail || t("onboarding.step2.testFailedTitle"));
      }
    } catch (e) {
      setTestErrorMsg(e instanceof Error ? e.message : String(e));
    } finally {
      setProviderBusy(false);
    }
  };

  const handleStep2Continue = async () => {
    if (apiKey.trim() !== "" || (!providerSavedMsg && presetGroup !== "fake" && native)) {
      await saveCurrentProvider();
    } else {
      onUpdateSettings({
        onboardingProviderId: resolvedProviderId,
        onboardingModel: providerModel.trim() || roleDefaults.defaultModel
      });
    }
    goToStep(3);
  };

  const handleStep3Continue = async () => {
    setSafetyError(null);
    onUpdateSettings({ onboardingPermissionPreset: safetyPreset });
    if (client) {
      setSafetyBusy(true);
      try {
        await client.putPolicy({ preset: safetyPreset });
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        setSafetyError(t("onboarding.step3.saveError", { message }));
        setSafetyBusy(false);
        return;
      }
      setSafetyBusy(false);
    }
    goToStep(4);
  };

  const handleAvatarChange = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) {
      return;
    }
    try {
      setBotAvatar(await avatarFromFile(file));
      setAvatarError(null);
    } catch (err) {
      setAvatarError(
        err instanceof AvatarFileError && err.reason === "tooLarge"
          ? t("botForm.avatarTooLarge")
          : t("botForm.avatarUnreadable")
      );
    }
  };

  const handleStep4Continue = async () => {
    setBotError(null);
    if (!botName.trim()) {
      setBotError(t("botForm.nameRequired"));
      return;
    }
    const execModel = executorModel.trim() || roleDefaults.executor;
    if (!execModel) {
      setBotError(t("botForm.executorRequired"));
      return;
    }

    if (!client) {
      setBotError(daemonError || t("onboarding.daemonBanner.offlineTitle"));
      return;
    }

    setBotBusy(true);
    try {
      const activeProviderForBot =
        fakeAvailable && presetGroup === "fake"
          ? FAKE_PROVIDER_ID
          : resolvedProviderId;
      const existing = createdBotId
        ? bots.find((b) => b.id === createdBotId)
        : undefined;
      const baseDraft = draftFromBot(existing);
      const config = configFromDraft(
        {
          ...baseDraft,
          name: botName.trim(),
          color: botColor,
          avatar: botAvatar ?? null,
          persona: botPersona,
          executor: { provider: activeProviderForBot, model: execModel },
          planner: {
            provider: plannerModel.trim() ? activeProviderForBot : "",
            model: plannerModel.trim()
          },
          classifier: {
            provider: classifierModel.trim() ? activeProviderForBot : "",
            model: classifierModel.trim()
          },
          preset: safetyPreset
        },
        existing
      );

      const input = { name: botName.trim(), config };
      const savedBot = existing
        ? await client.updateBot(existing.id, input)
        : await client.createBot(input);

      setCreatedBotId(savedBot.id);
      onUpdateSettings({ onboardingBotId: savedBot.id });
      await refreshBots();
      goToStep(5);
    } catch (e) {
      setBotError(e instanceof Error ? e.message : String(e));
    } finally {
      setBotBusy(false);
    }
  };

  const runSamplePrompt = async (displayPrompt: string, wirePrompt: string) => {
    if (!client) {
      setTryError(daemonError || t("onboarding.daemonBanner.offlineTitle"));
      return;
    }
    const targetBotId = createdBotId || bots[0]?.id;
    if (!targetBotId) {
      setTryError(t("settings.permissions.noBots"));
      return;
    }

    setTryError(null);
    setTryBusy(true);
    setLastTriedPrompt({ display: displayPrompt, wire: wirePrompt });
    promptAliasRef.current[wirePrompt] = displayPrompt;

    try {
      let chatId = tryChatId;
      if (!chatId) {
        const created = await client.createChat(targetBotId, displayPrompt);
        chatId = created.id;
        setTryChatId(chatId);
        await refreshChats();
      }
      await client.sendMessage(chatId, wirePrompt);
      const msgs = await client.listMessages(chatId);
      setTryMessages(msgs);
      await refreshRuns();
    } catch (e) {
      setTryError(e instanceof Error ? e.message : String(e));
      setTryBusy(false);
    }
  };

  const renderTryTimelineItem = (item: TimelineItem) => {
    switch (item.kind) {
      case "text":
        return item.text ? (
          <div key={item.key} className={styles.msgBubbleBot}>
            {item.text}
          </div>
        ) : null;
      case "tool":
        return item.status === "awaiting_approval" ? (
          <ApprovalCard
            key={item.key}
            tool={item}
            onDecide={async (decision, scope, reason) => {
              if (!client) {
                return;
              }
              await client.decideApproval(
                item.callId,
                decision,
                scope,
                reason
              );
            }}
          />
        ) : (
          <ToolCard key={item.key} tool={item} />
        );
      default:
        return null;
    }
  };

  const usesFakeRuntime =
    fakeAvailable ||
    presetGroup === "fake" ||
    resolvedProviderId === FAKE_PROVIDER_ID;

  const hasKeySaved = Boolean(keyStoredFor[resolvedProviderId]);
  const activeBotId = createdBotId || bots[0]?.id;

  return (
    <div className={styles.pane} data-testid="onboarding-view">
      <div className={styles.container}>
        <div className={styles.topBar}>
          <div className={styles.topMeta}>
            <span className={styles.badge}>{t("onboarding.badge")}</span>
            <span className={styles.stepCounter}>
              {t("onboarding.stepCounter", {
                current: step,
                total: TOTAL_STEPS
              })}
            </span>
          </div>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => onSkip(step)}
            data-testid="onboarding-skip"
          >
            {t("onboarding.skip")}
          </Button>
        </div>

        <ol className={styles.stepper} aria-label={t("onboarding.badge")}>
          {([1, 2, 3, 4, 5] as const).map((idx) => {
            const isActive = step === idx;
            const isDone = step > idx;
            return (
              <li key={idx}>
                <button
                  type="button"
                  className={[
                    styles.stepButton,
                    isActive ? styles.stepButtonActive : "",
                    isDone ? styles.stepButtonDone : ""
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  aria-current={isActive ? "step" : undefined}
                  onClick={() => goToStep(idx)}
                >
                  <span className={styles.stepIndex}>{idx}</span>
                  <span className={styles.stepLabel}>
                    {t(`onboarding.steps.${idx}`)}
                  </span>
                </button>
              </li>
            );
          })}
        </ol>

        {phase === "error" ? (
          <div className={styles.daemonBanner} role="alert">
            <div className={styles.daemonBannerText}>
              <p className={styles.daemonBannerTitle}>
                {t("onboarding.daemonBanner.offlineTitle")}
              </p>
              <p className={styles.daemonBannerHint}>
                {daemonError || t("onboarding.daemonBanner.offlineHint")}
              </p>
            </div>
            <Button size="sm" variant="secondary" onClick={retry}>
              {t("onboarding.daemonBanner.retry")}
            </Button>
          </div>
        ) : null}

        {step === 1 ? (
          <Card padding="lg" className={styles.stepCard}>
            <header className={styles.header}>
              <h1 className={styles.title}>{t("onboarding.step1.title")}</h1>
              <p className={styles.subtitle}>
                {t("onboarding.step1.subtitle")}
              </p>
            </header>

            <section className={styles.section}>
              <h2 className={styles.sectionTitle}>
                {t("onboarding.step1.languageTitle")}
              </h2>
              <p className={formStyles.hint}>
                {t("onboarding.step1.languageHint")}
              </p>
              <div className={styles.optionRow}>
                {(["en", "tr"] as const).map((lang) => (
                  <Button
                    key={lang}
                    variant="secondary"
                    active={settings.language === lang}
                    onClick={() => onLanguageChange(lang)}
                    data-testid={`onboarding-lang-${lang}`}
                  >
                    {t(`settings.language.${lang}`)}
                  </Button>
                ))}
              </div>
            </section>

            <section className={styles.section}>
              <h2 className={styles.sectionTitle}>
                {t("onboarding.step1.themeTitle")}
              </h2>
              <p className={formStyles.hint}>
                {t("onboarding.step1.themeHint")}
              </p>
              <ThemePicker
                mode={settings.themeMode}
                custom={settings.customTheme}
                accent={settings.accentColor}
                onChange={onUpdateSettings}
              />
            </section>

            <div className={styles.privacyBox}>
              <h3 className={styles.privacyTitle}>
                {t("onboarding.step1.privacyTitle")}
              </h3>
              <p className={styles.privacyBody}>
                {t("onboarding.step1.privacyBody")}
              </p>
            </div>

            <footer className={styles.footer}>
              <Button variant="ghost" onClick={() => onSkip(1)}>
                {t("onboarding.skip")}
              </Button>
              <div className={styles.footerRight}>
                <Button
                  variant="primary"
                  onClick={() => goToStep(2)}
                  data-testid="onboarding-next"
                >
                  {t("onboarding.next")}
                </Button>
              </div>
            </footer>
          </Card>
        ) : null}

        {step === 2 ? (
          <Card padding="lg" className={styles.stepCard}>
            <header className={styles.header}>
              <h1 className={styles.title}>{t("onboarding.step2.title")}</h1>
              <p className={styles.subtitle}>
                {t("onboarding.step2.subtitle")}
              </p>
            </header>

            <div className={styles.presetGrid}>
              {(
                [
                  "gemini",
                  "anthropic",
                  "openai_compatible",
                  "ollama",
                  ...(fakeAvailable || import.meta.env.DEV
                    ? (["fake"] as const)
                    : [])
                ] as OnboardingPresetGroup[]
              ).map((group) => {
                const isSelected = presetGroup === group;
                return (
                  <button
                    key={group}
                    type="button"
                    className={[
                      styles.presetCard,
                      isSelected ? styles.presetCardActive : ""
                    ]
                      .filter(Boolean)
                      .join(" ")}
                    onClick={() => selectPresetGroup(group)}
                    data-testid={`onboarding-preset-${group}`}
                  >
                    <div className={styles.presetHeader}>
                      <span className={styles.presetTitle}>
                        {t(`onboarding.step2.presets.${group}.title`)}
                      </span>
                      <span className={styles.presetBadge}>
                        {t(`onboarding.step2.presets.${group}.badge`)}
                      </span>
                    </div>
                    <p className={styles.presetDescription}>
                      {t(`onboarding.step2.presets.${group}.description`)}
                    </p>
                  </button>
                );
              })}
            </div>

            {presetGroup === "gemini" ? (
              <p className={styles.freeTierNotice} data-testid="gemini-free-tier-note">
                {t("onboarding.step2.presets.gemini.freeTierLine")}
              </p>
            ) : null}

            {presetGroup === "openai_compatible" ? (
              <div className={formStyles.row}>
                <div className={formStyles.field}>
                  <label className={formStyles.label} htmlFor="onb-openai-choice">
                    {t("onboarding.step2.providerId")}
                  </label>
                  <select
                    id="onb-openai-choice"
                    className={formStyles.select}
                    value={openAiChoice}
                    onChange={(e) => handleOpenAiSubChoice(e.target.value)}
                  >
                    {OPENAI_COMPAT_CATALOG.map((id) => (
                      <option key={id} value={id}>
                        {id}
                      </option>
                    ))}
                    <option value={CUSTOM_OPENAI_ID}>
                      {t("onboarding.step2.customOption")}
                    </option>
                  </select>
                </div>

                {openAiChoice === CUSTOM_OPENAI_ID ? (
                  <>
                    <div className={`${formStyles.field} ${formStyles.grow}`}>
                      <label className={formStyles.label} htmlFor="onb-custom-id">
                        {t("onboarding.step2.customId")}
                      </label>
                      <input
                        id="onb-custom-id"
                        className={formStyles.input}
                        value={customProviderId}
                        onChange={(e) => setCustomProviderId(e.target.value)}
                      />
                    </div>
                    <div className={`${formStyles.field} ${formStyles.grow}`}>
                      <label className={formStyles.label} htmlFor="onb-base-url">
                        {t("onboarding.step2.baseUrl")}
                      </label>
                      <input
                        id="onb-base-url"
                        className={formStyles.input}
                        placeholder="https://api.example.com/v1"
                        value={baseUrl}
                        onChange={(e) => setBaseUrl(e.target.value)}
                      />
                    </div>
                  </>
                ) : null}
              </div>
            ) : null}

            {presetGroup === "ollama" ? (
              <div className={formStyles.field}>
                <label className={formStyles.label} htmlFor="onb-ollama-url">
                  {t("onboarding.step2.baseUrl")}
                </label>
                <input
                  id="onb-ollama-url"
                  className={formStyles.input}
                  value={baseUrl}
                  onChange={(e) => setBaseUrl(e.target.value)}
                />
                <p className={formStyles.hint}>
                  {t("onboarding.step2.noKeyNeeded")}
                </p>
              </div>
            ) : null}

            <div className={formStyles.row}>
              {roleDefaults.requiresKey ? (
                <div className={`${formStyles.field} ${formStyles.grow}`}>
                  <label className={formStyles.label} htmlFor="onb-api-key">
                    {t("onboarding.step2.apiKey")}
                  </label>
                  {hasKeySaved && !editingKey ? (
                    <div
                      className={styles.keySavedRow}
                      data-testid="onboarding-key-saved-badge"
                    >
                      <span className={styles.keySavedText}>
                        {t("onboarding.step2.keyStoredBadge")}
                      </span>
                      <Button
                        size="sm"
                        variant="ghost"
                        onClick={() => {
                          setApiKey("");
                          setEditingKey(true);
                        }}
                      >
                        {t("onboarding.step2.replaceKey")}
                      </Button>
                    </div>
                  ) : (
                    <input
                      id="onb-api-key"
                      className={formStyles.input}
                      type="password"
                      autoComplete="off"
                      placeholder={t("onboarding.step2.apiKeyPlaceholder")}
                      value={apiKey}
                      onChange={(e) => setApiKey(e.target.value)}
                    />
                  )}
                  <p className={formStyles.hint}>
                    {t("settings.providers.keyHint")}
                  </p>
                </div>
              ) : null}

              <div className={`${formStyles.field} ${formStyles.grow}`}>
                <label className={formStyles.label} htmlFor="onb-default-model">
                  {t("onboarding.step2.defaultModel")}
                </label>
                <input
                  id="onb-default-model"
                  className={formStyles.input}
                  value={providerModel}
                  onChange={(e) => setProviderModel(e.target.value)}
                />
              </div>
            </div>

            <div className={styles.optionRow}>
              {presetGroup !== "fake" ? (
                <Button
                  variant="secondary"
                  disabled={providerBusy}
                  onClick={() => void saveCurrentProvider()}
                >
                  {providerBusy
                    ? t("onboarding.step2.saving")
                    : t("onboarding.step2.saveProvider")}
                </Button>
              ) : null}
              <Button
                variant="secondary"
                disabled={providerBusy}
                onClick={() => void handleTestProvider()}
                data-testid="onboarding-test-provider"
              >
                {providerBusy
                  ? t("onboarding.step2.testing")
                  : t("onboarding.step2.testButton")}
              </Button>
            </div>

            {providerSavedMsg ? (
              <p className={formStyles.hint} role="status">
                {providerSavedMsg}
              </p>
            ) : null}

            {testSuccessMsg ? (
              <pre
                className={styles.statusOk}
                role="status"
                data-testid="onboarding-test-ok"
              >
                {testSuccessMsg}
              </pre>
            ) : null}

            {testErrorMsg ? (
              <div
                className={styles.errorCallout}
                role="alert"
                data-testid="onboarding-test-error"
              >
                <p className={styles.errorTitle}>
                  {t("onboarding.step2.testFailedTitle")}
                </p>
                <p className={styles.errorDetail}>{testErrorMsg}</p>
                <p className={formStyles.hint}>
                  {t("onboarding.step2.testFailedHint")}
                </p>
                <div>
                  <Button
                    size="sm"
                    variant="secondary"
                    disabled={providerBusy}
                    onClick={() => void handleTestProvider()}
                  >
                    {t("onboarding.step2.retryTest")}
                  </Button>
                </div>
              </div>
            ) : null}

            <footer className={styles.footer}>
              <Button variant="secondary" onClick={() => goToStep(1)}>
                {t("onboarding.back")}
              </Button>
              <div className={styles.footerRight}>
                <Button variant="ghost" onClick={() => onSkip(2)}>
                  {t("onboarding.skip")}
                </Button>
                <Button
                  variant="primary"
                  disabled={providerBusy}
                  onClick={() => void handleStep2Continue()}
                  data-testid="onboarding-next"
                >
                  {t("onboarding.next")}
                </Button>
              </div>
            </footer>
          </Card>
        ) : null}

        {step === 3 ? (
          <Card padding="lg" className={styles.stepCard}>
            <header className={styles.header}>
              <h1 className={styles.title}>{t("onboarding.step3.title")}</h1>
              <p className={styles.subtitle}>
                {t("onboarding.step3.subtitle")}
              </p>
            </header>

            <div className={styles.safetyList}>
              {SAFETY_PRESETS.map((preset) => {
                const isSelected = safetyPreset === preset;
                return (
                  <label
                    key={preset}
                    className={[
                      styles.safetyOption,
                      isSelected ? styles.safetyOptionActive : ""
                    ]
                      .filter(Boolean)
                      .join(" ")}
                  >
                    <input
                      type="radio"
                      name="onboarding-safety-preset"
                      checked={isSelected}
                      onChange={() => setSafetyPreset(preset)}
                    />
                    <span className={styles.safetyOptionBody}>
                      <span className={styles.safetyOptionHead}>
                        <span className={styles.safetyOptionName}>
                          {t(`botForm.preset.${preset}.name`)}
                        </span>
                        {preset === "approve-destructive" ? (
                          <span className={styles.badge}>
                            {t("onboarding.step3.recommended")}
                          </span>
                        ) : null}
                      </span>
                      <span className={formStyles.hint}>
                        {t(`botForm.preset.${preset}.hint`)}
                      </span>
                    </span>
                  </label>
                );
              })}
            </div>

            <div className={styles.privacyBox}>
              <h2 className={styles.privacyTitle}>
                {t("onboarding.step3.whatItMeansTitle")}
              </h2>
              <p className={styles.privacyBody}>
                {t(`onboarding.step3.explain.${safetyPreset}`)}
              </p>
            </div>

            <div className={styles.privacyBox}>
              <h2 className={styles.privacyTitle}>
                {t("onboarding.step3.floorsTitle")}
              </h2>
              <ul className={styles.floorsList}>
                <li>{t("onboarding.step3.floorMoney")}</li>
                <li>{t("onboarding.step3.floorNever")}</li>
                <li>
                  {t("onboarding.step3.floorHotkey", {
                    hotkey: settings.stopHotkey || DEFAULT_STOP_HOTKEY
                  })}
                </li>
              </ul>
            </div>

            {safetyError ? (
              <div className={styles.errorCallout} role="alert">
                <p className={styles.errorDetail}>{safetyError}</p>
                <div className={styles.optionRow}>
                  <Button
                    size="sm"
                    variant="secondary"
                    onClick={() => void handleStep3Continue()}
                  >
                    {t("main.daemonRetry")}
                  </Button>
                </div>
              </div>
            ) : null}

            <footer className={styles.footer}>
              <Button variant="secondary" onClick={() => goToStep(2)}>
                {t("onboarding.back")}
              </Button>
              <div className={styles.footerRight}>
                <Button variant="ghost" onClick={() => onSkip(3)}>
                  {t("onboarding.skip")}
                </Button>
                <Button
                  variant="primary"
                  disabled={safetyBusy}
                  onClick={() => void handleStep3Continue()}
                  data-testid="onboarding-next"
                >
                  {safetyBusy
                    ? t("onboarding.step3.saving")
                    : t("onboarding.next")}
                </Button>
              </div>
            </footer>
          </Card>
        ) : null}

        {step === 4 ? (
          <Card padding="lg" className={styles.stepCard}>
            <header className={styles.header}>
              <h1 className={styles.title}>{t("onboarding.step4.title")}</h1>
              <p className={styles.subtitle}>
                {t("onboarding.step4.subtitle")}
              </p>
            </header>

            <div className={formStyles.row}>
              <BotAvatar bot={{ id: "new-bot", name: botName, color: botColor, avatar: botAvatar }} size="lg" />
              <div className={`${formStyles.field} ${formStyles.grow}`}>
                <label className={formStyles.label} htmlFor="onb-bot-name">
                  {t("botForm.name")}
                </label>
                <input
                  id="onb-bot-name"
                  className={formStyles.input}
                  value={botName}
                  onChange={(e) => setBotName(e.target.value)}
                  data-testid="onboarding-bot-name"
                />
              </div>
            </div>

            <div className={formStyles.field}>
              <span className={formStyles.label}>{t("botForm.avatar")}</span>
              <input
                ref={avatarInputRef}
                type="file"
                accept="image/png,image/jpeg,image/webp,image/gif"
                className={styles.hiddenFile}
                onChange={(e) => void handleAvatarChange(e)}
              />
              <div className={styles.optionRow}>
                <Button
                  size="sm"
                  variant="secondary"
                  onClick={() => avatarInputRef.current?.click()}
                >
                  {t("botForm.avatarChoose")}
                </Button>
                {botAvatar ? (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => setBotAvatar(undefined)}
                  >
                    {t("botForm.avatarUseInitials")}
                  </Button>
                ) : null}
              </div>
              {avatarError ? (
                <p className={formStyles.error}>{avatarError}</p>
              ) : null}
            </div>

            <div className={formStyles.field}>
              <span className={formStyles.label}>{t("botForm.color")}</span>
              <div className={styles.swatches}>
                {BOT_COLORS.map((swatch: string) => (
                  <button
                    key={swatch}
                    type="button"
                    className={[
                      styles.swatch,
                      botColor.toLowerCase() === swatch.toLowerCase()
                        ? styles.swatchActive
                        : ""
                    ]
                      .filter(Boolean)
                      .join(" ")}
                    style={{ "--swatch-color": swatch } as CSSProperties}
                    aria-label={swatch}
                    onClick={() => setBotColor(swatch)}
                  />
                ))}
                <input
                  type="color"
                  className={styles.colorInput}
                  value={botColor}
                  aria-label={t("botForm.colorCustom")}
                  onChange={(e) => setBotColor(e.target.value)}
                />
              </div>
            </div>

            <div className={formStyles.field}>
              <label className={formStyles.label} htmlFor="onb-bot-persona">
                {t("botForm.persona")}
              </label>
              <textarea
                id="onb-bot-persona"
                className={formStyles.textarea}
                value={botPersona}
                onChange={(e) => setBotPersona(e.target.value)}
              />
            </div>

            <div className={styles.section}>
              <h2 className={styles.sectionTitle}>
                {t("onboarding.step4.rolesTitle", {
                  provider: resolvedProviderId
                })}
              </h2>
              <p className={formStyles.hint}>
                {t("onboarding.step4.rolesHint")}
              </p>
              <div className={formStyles.row}>
                <div className={`${formStyles.field} ${formStyles.grow}`}>
                  <label className={formStyles.label} htmlFor="onb-role-exec">
                    {t("botForm.role.executor")}
                  </label>
                  <input
                    id="onb-role-exec"
                    className={formStyles.input}
                    value={executorModel}
                    onChange={(e) => setExecutorModel(e.target.value)}
                  />
                </div>
                <div className={`${formStyles.field} ${formStyles.grow}`}>
                  <label className={formStyles.label} htmlFor="onb-role-plan">
                    {t("botForm.role.planner")}
                  </label>
                  <input
                    id="onb-role-plan"
                    className={formStyles.input}
                    value={plannerModel}
                    onChange={(e) => setPlannerModel(e.target.value)}
                  />
                </div>
                <div className={`${formStyles.field} ${formStyles.grow}`}>
                  <label className={formStyles.label} htmlFor="onb-role-class">
                    {t("botForm.role.classifier")}
                  </label>
                  <input
                    id="onb-role-class"
                    className={formStyles.input}
                    value={classifierModel}
                    onChange={(e) => setClassifierModel(e.target.value)}
                  />
                </div>
              </div>
            </div>

            {botError ? (
              <div className={styles.errorCallout} role="alert">
                <p className={styles.errorDetail}>{botError}</p>
              </div>
            ) : null}

            <footer className={styles.footer}>
              <Button variant="secondary" onClick={() => goToStep(3)}>
                {t("onboarding.back")}
              </Button>
              <div className={styles.footerRight}>
                <Button variant="ghost" onClick={() => onSkip(4)}>
                  {t("onboarding.skip")}
                </Button>
                <Button
                  variant="primary"
                  disabled={botBusy}
                  onClick={() => void handleStep4Continue()}
                  data-testid="onboarding-next"
                >
                  {botBusy
                    ? t("onboarding.step4.creating")
                    : t("onboarding.next")}
                </Button>
              </div>
            </footer>
          </Card>
        ) : null}

        {step === 5 ? (
          <Card padding="lg" className={styles.stepCard}>
            <header className={styles.header}>
              <h1 className={styles.title}>{t("onboarding.step5.title")}</h1>
              <p className={styles.subtitle}>
                {t("onboarding.step5.subtitle")}
              </p>
            </header>

            {activeBotId ? (
              <div className={styles.keySavedRow}>
                <span className={styles.keySavedText}>
                  {t("onboarding.step4.createdBadge", {
                    name:
                      bots.find((b) => b.id === activeBotId)?.name || botName
                  })}
                </span>
              </div>
            ) : null}

            <div className={styles.promptButtons}>
              <Button
                variant="secondary"
                disabled={tryBusy}
                onClick={() => {
                  const display = t("onboarding.step5.promptScreen");
                  const wire = usesFakeRuntime
                    ? 'call:use_tool {"name":"fs_list","input":{"path":"."}}'
                    : display;
                  void runSamplePrompt(display, wire);
                }}
                data-testid="onboarding-try-screen"
              >
                {t("onboarding.step5.promptScreen")}
              </Button>
              <Button
                variant="primary"
                disabled={tryBusy}
                onClick={() => {
                  const display = t("onboarding.step5.promptNotepad");
                  const wire = usesFakeRuntime
                    ? 'call:use_tool {"name":"shell_run","input":{"command":"notepad.exe"}}'
                    : display;
                  void runSamplePrompt(display, wire);
                }}
                data-testid="onboarding-try-notepad"
              >
                {t("onboarding.step5.promptNotepad")}
              </Button>
            </div>

            <div className={formStyles.row}>
              <div className={`${formStyles.field} ${formStyles.grow}`}>
                <input
                  className={formStyles.input}
                  placeholder={t("onboarding.step5.customPlaceholder")}
                  value={customPrompt}
                  onChange={(e) => setCustomPrompt(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && customPrompt.trim() && !tryBusy) {
                      e.preventDefault();
                      const text = customPrompt.trim();
                      setCustomPrompt("");
                      void runSamplePrompt(text, text);
                    }
                  }}
                />
              </div>
              <Button
                variant="secondary"
                disabled={!customPrompt.trim() || tryBusy}
                onClick={() => {
                  const text = customPrompt.trim();
                  setCustomPrompt("");
                  void runSamplePrompt(text, text);
                }}
              >
                {tryBusy
                  ? t("onboarding.step5.sending")
                  : t("onboarding.step5.send")}
              </Button>
            </div>

            <p className={formStyles.hint}>
              {t("onboarding.step5.approvalHint")}
            </p>

            <div
              className={styles.liveStreamBox}
              data-testid="onboarding-live-stream"
            >
              {tryMessages.length === 0 && tryChatRuns.length === 0 ? (
                <p className={formStyles.hint}>
                  {t("onboarding.step5.emptyHint")}
                </p>
              ) : null}

              {tryMessages
                .filter((m) => m.role === "user")
                .map((m) => {
                  const displayText =
                    promptAliasRef.current[m.content] || m.content;
                  return (
                    <div key={m.id} className={styles.msgBubbleUser}>
                      {displayText}
                    </div>
                  );
                })}

              {tryChatRuns.flatMap((run) =>
                run.items.map((item) => renderTryTimelineItem(item))
              )}
            </div>

            {tryError ? (
              <div className={styles.errorCallout} role="alert">
                <p className={styles.errorDetail}>{tryError}</p>
                {lastTriedPrompt ? (
                  <div>
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={() =>
                        void runSamplePrompt(
                          lastTriedPrompt.display,
                          lastTriedPrompt.wire
                        )
                      }
                    >
                      {t("onboarding.step5.retryPrompt")}
                    </Button>
                  </div>
                ) : null}
              </div>
            ) : null}

            <footer className={styles.footer}>
              <Button variant="secondary" onClick={() => goToStep(4)}>
                {t("onboarding.back")}
              </Button>
              <div className={styles.footerRight}>
                {tryChatId && activeBotId ? (
                  <Button
                    variant="secondary"
                    onClick={() =>
                      onComplete({ botId: activeBotId, chatId: tryChatId })
                    }
                  >
                    {t("onboarding.step5.openFullChat")}
                  </Button>
                ) : null}
                <Button
                  variant="primary"
                  onClick={() =>
                    onComplete({
                      botId: activeBotId,
                      chatId: tryChatId || undefined
                    })
                  }
                  data-testid="onboarding-finish"
                >
                  {t("onboarding.finish")}
                </Button>
              </div>
            </footer>
          </Card>
        ) : null}
      </div>
    </div>
  );
}
