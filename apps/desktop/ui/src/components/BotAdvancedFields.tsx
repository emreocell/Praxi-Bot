import { type ChangeEvent, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ApiClient } from "../api/client";
import {
  type McpServer,
  PERMISSION_CATEGORIES,
  type PermissionCategoryName,
  type PermissionLevel,
  type ProvidersDto
} from "../api/types";
import { avatarFromFile, AvatarFileError } from "../lib/avatar";
import {
  type BotDraft,
  type ModelRef,
  PRESET_CHOICES,
  type PresetChoice
} from "../lib/botProfile";
import { brandIcon } from "../lib/brandIcons";
import { botHasAccess } from "../lib/mcpApps";
import { FAKE_PROVIDER_ID } from "../lib/providers";
import { isCliProvider, nonCliProviderIds, providersView } from "../lib/providerView";
import { BrandMark } from "./BrandMark";
import formStyles from "./Form.module.css";
import styles from "./BotAdvancedFields.module.css";

const LEVELS: readonly PermissionLevel[] = ["deny", "ask", "allow"] as const;

/** Load provider catalog and per-app access snapshot when a bot dialog/form opens. */
export function useBotFormDeps(
  client: ApiClient | null,
  existingId: string | undefined,
  fakeProvider: boolean,
  active: boolean
) {
  const [servers, setServers] = useState<McpServer[] | null>(null);
  const [allowed, setAllowed] = useState<Record<string, boolean>>({});
  const [initialApps, setInitialApps] = useState<Record<string, boolean>>({});
  const [providersDto, setProvidersDto] = useState<ProvidersDto | null>(null);

  useEffect(() => {
    if (!client || !active) {
      return undefined;
    }
    let cancelled = false;
    client
      .providers()
      .then((dto) => {
        if (!cancelled) {
          setProvidersDto(dto);
        }
      })
      .catch(() => {});
    client
      .listMcpServers()
      .then((list) => {
        if (!cancelled) {
          const snapshot = Object.fromEntries(
            list.map((s) => [s.name, botHasAccess(s, existingId)])
          );
          setServers(list);
          setAllowed(snapshot);
          setInitialApps(snapshot);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setServers([]);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [client, existingId, active]);

  const providerList = useMemo(() => providersView(providersDto), [providersDto]);
  const providerIds = useMemo(() => {
    const ids = new Set<string>(providerList.selectable);
    if (fakeProvider) {
      ids.add(FAKE_PROVIDER_ID);
    }
    return [...ids];
  }, [providerList.selectable, fakeProvider]);
  const httpProviders = useMemo(
    () => nonCliProviderIds(providerIds, providersDto),
    [providerIds, providersDto]
  );

  return {
    providersDto,
    providerIds,
    httpProviders,
    servers,
    setServers,
    allowed,
    setAllowed,
    initialApps,
    setInitialApps
  };
}

/** Provider + model input pair for a single role (`executor`, `planner`, `classifier`). */
export function ModelRoleField({
  label,
  value,
  providers,
  providersDto,
  required,
  onChange
}: {
  label: string;
  value: ModelRef;
  providers: string[];
  providersDto: ProvidersDto | null;
  required?: boolean;
  onChange: (v: ModelRef) => void;
}) {
  const { t } = useTranslation();
  const known = providers.includes(value.provider) || value.provider === "";
  const options = known ? providers : [value.provider, ...providers];
  const cli = isCliProvider(value.provider, providersDto);
  return (
    <div className={formStyles.field}>
      <span className={formStyles.label}>
        {label}
        {required ? " *" : ""}
      </span>
      <div className={formStyles.row}>
        <select
          className={formStyles.select}
          value={value.provider}
          aria-label={`${label}: ${t("botForm.provider")}`}
          onChange={(e) => {
            const next = e.target.value;
            onChange({
              provider: next,
              model: isCliProvider(next, providersDto)
                ? "default"
                : cli
                  ? ""
                  : value.model
            });
          }}
        >
          <option value="">{t("botForm.defaultProvider")}</option>
          {options.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>
        {cli ? (
          <select
            className={`${formStyles.select} ${formStyles.grow}`}
            value={value.model || "default"}
            aria-label={`${label}: ${t("botForm.model")}`}
            onChange={(e) => onChange({ ...value, model: e.target.value })}
          >
            <option value="default">default</option>
          </select>
        ) : (
          <input
            className={`${formStyles.input} ${formStyles.grow}`}
            value={value.model}
            placeholder={t("botForm.modelPlaceholder")}
            aria-label={`${label}: ${t("botForm.model")}`}
            onChange={(e) => onChange({ ...value, model: e.target.value })}
          />
        )}
      </div>
    </div>
  );
}

/** Custom uploaded avatar file control (PNG, JPEG, WebP, GIF) with reset back to 3D character. */
export function BotAvatarUploadField({
  avatar,
  onChange
}: {
  avatar: string | null;
  onChange: (avatar: string | null) => void;
}) {
  const { t } = useTranslation();
  const fileRef = useRef<HTMLInputElement | null>(null);
  const [avatarError, setAvatarError] = useState<string | null>(null);

  const onFile = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) {
      return;
    }
    setAvatarError(null);
    try {
      onChange(await avatarFromFile(file));
    } catch (err) {
      setAvatarError(
        err instanceof AvatarFileError && err.reason === "tooLarge"
          ? t("botForm.avatarTooLarge")
          : t("botForm.avatarUnreadable")
      );
    }
  };

  return (
    <div className={formStyles.field}>
      <span className={formStyles.label}>{t("botForm.avatar")}</span>
      <div className={formStyles.row}>
        <input
          ref={fileRef}
          type="file"
          accept="image/png,image/jpeg,image/webp,image/gif"
          className={styles.hiddenFile}
          onChange={(e) => void onFile(e)}
        />
        <Button size="sm" variant="secondary" onClick={() => fileRef.current?.click()}>
          {t("botForm.avatarChoose")}
        </Button>
        {avatar ? (
          <Button size="sm" variant="ghost" onClick={() => onChange(null)}>
            {t("botForm.avatarUseInitials")}
          </Button>
        ) : (
          <span className={formStyles.hint}>{t("botForm.avatarInitialsHint")}</span>
        )}
      </div>
      {avatarError ? (
        <p className={formStyles.error}>{avatarError}</p>
      ) : (
        <p className={formStyles.hint}>{t("botForm.avatarHint")}</p>
      )}
    </div>
  );
}

/** Explicit executor / planner / classifier / fallback fields. */
export function BotModelRolesFields({
  draft,
  providerIds,
  httpProviders,
  providersDto,
  executorMissing,
  patch
}: {
  draft: BotDraft;
  providerIds: string[];
  httpProviders: string[];
  providersDto: ProvidersDto | null;
  executorMissing: boolean;
  patch: (p: Partial<BotDraft>) => void;
}) {
  const { t } = useTranslation();
  return (
    <div className={styles.sectionGroup}>
      <p className={formStyles.hint}>{t("botForm.modelsHint")}</p>
      <ModelRoleField
        label={t("botForm.role.executor")}
        required
        value={draft.executor}
        providers={providerIds}
        providersDto={providersDto}
        onChange={(executor) => patch({ executor })}
      />
      {executorMissing ? (
        <p className={formStyles.hint}>{t("botForm.executorRequired")}</p>
      ) : null}
      <ModelRoleField
        label={t("botForm.role.planner")}
        value={draft.planner}
        providers={httpProviders}
        providersDto={providersDto}
        onChange={(planner) => patch({ planner })}
      />
      <ModelRoleField
        label={t("botForm.role.classifier")}
        value={draft.classifier}
        providers={httpProviders}
        providersDto={providersDto}
        onChange={(classifier) => patch({ classifier })}
      />
      <div className={formStyles.field}>
        <label className={formStyles.label} htmlFor="bot-fallback">
          {t("botForm.role.fallback")}
        </label>
        <input
          id="bot-fallback"
          className={formStyles.input}
          value={draft.fallback}
          placeholder={t("botForm.fallbackPlaceholder")}
          onChange={(e) => patch({ fallback: e.target.value })}
        />
      </div>
    </div>
  );
}

/** Existing 5 daemon permission presets plus 10-category custom matrix. */
export function BotPermissionsFields({
  draft,
  patch
}: {
  draft: BotDraft;
  patch: (p: Partial<BotDraft>) => void;
}) {
  const { t } = useTranslation();
  return (
    <div className={styles.sectionGroup}>
      <div
        className={styles.presets}
        role="radiogroup"
        aria-label={t("botForm.permissions")}
      >
        {PRESET_CHOICES.map((p: PresetChoice) => (
          <label
            key={p}
            className={[styles.preset, draft.preset === p ? styles.presetActive : ""]
              .filter(Boolean)
              .join(" ")}
          >
            <input
              type="radio"
              name="preset"
              value={p}
              checked={draft.preset === p}
              onChange={() => patch({ preset: p })}
            />
            <span className={styles.presetText}>
              <span className={styles.presetName}>{t(`botForm.preset.${p}.name`)}</span>
              <span className={formStyles.hint}>{t(`botForm.preset.${p}.hint`)}</span>
            </span>
          </label>
        ))}
      </div>
      {draft.preset === "custom" ? (
        <div className={styles.categories}>
          {PERMISSION_CATEGORIES.map((c: PermissionCategoryName) => (
            <div key={c} className={formStyles.field}>
              <label className={formStyles.label} htmlFor={`cat-${c}`}>
                {t(`botForm.category.${c}`)}
              </label>
              <select
                id={`cat-${c}`}
                className={formStyles.select}
                value={draft.categories[c]}
                onChange={(e) =>
                  patch({
                    categories: {
                      ...draft.categories,
                      [c]: e.target.value as PermissionLevel
                    }
                  })
                }
              >
                {LEVELS.map((l) => (
                  <option key={l} value={l}>
                    {t(`botForm.level.${l}`)}
                  </option>
                ))}
              </select>
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}

/** Connected MCP app access checkboxes for a bot. */
export function BotAppsFields({
  servers,
  allowed,
  onToggle
}: {
  servers: McpServer[] | null;
  allowed: Record<string, boolean>;
  onToggle: (serverName: string, checked: boolean) => void;
}) {
  const { t } = useTranslation();
  if (servers !== null && servers.length === 0) {
    return <p className={formStyles.hint}>{t("botForm.apps.none")}</p>;
  }
  return (
    <div className={styles.sectionGroup}>
      <p className={formStyles.hint}>{t("botForm.apps.hint")}</p>
      <ul className={styles.appList} data-testid="bot-apps">
        {(servers ?? []).map((s) => (
          <li key={s.name}>
            <label className={styles.appItem}>
              <input
                type="checkbox"
                className={styles.appCheck}
                checked={allowed[s.name] ?? false}
                data-testid={`bot-app-${s.name}`}
                onChange={(e) => onToggle(s.name, e.target.checked)}
              />
              <BrandMark name={s.name} size={18} />
              <span className={styles.appName}>
                {brandIcon(s.name).known ? brandIcon(s.name).name : s.name}
              </span>
            </label>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** Computer & browser execution environment selectors. */
export function BotRuntimeFields({
  draft,
  patch
}: {
  draft: BotDraft;
  patch: (p: Partial<BotDraft>) => void;
}) {
  const { t } = useTranslation();
  return (
    <div className={styles.sectionGroup}>
      <div className={formStyles.row}>
        <div className={`${formStyles.field} ${formStyles.grow}`}>
          <label className={formStyles.label} htmlFor="bot-computer">
            {t("botForm.computer")}
          </label>
          <select
            id="bot-computer"
            className={formStyles.select}
            value={draft.computer}
            onChange={() => patch({ computer: "local" })}
          >
            <option value="local">{t("botForm.local")}</option>
            <option value="remote" disabled>
              {t("botForm.remoteSoon")}
            </option>
          </select>
        </div>
        <div className={`${formStyles.field} ${formStyles.grow}`}>
          <label className={formStyles.label} htmlFor="bot-browser">
            {t("botForm.browser")}
          </label>
          <select
            id="bot-browser"
            className={formStyles.select}
            value={draft.browser}
            onChange={() => patch({ browser: "local" })}
          >
            <option value="local">{t("botForm.local")}</option>
            <option value="remote" disabled>
              {t("botForm.remoteSoon")}
            </option>
          </select>
        </div>
      </div>
      <p className={formStyles.hint}>{t("botForm.runtimeHint")}</p>
    </div>
  );
}
