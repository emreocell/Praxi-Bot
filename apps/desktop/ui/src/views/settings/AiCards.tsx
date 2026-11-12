import type { TFunction } from "i18next";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { MOCK_BASE_URL } from "../../api/mockMode";
import formStyles from "../../components/Form.module.css";
import { ModelPicker } from "../../components/ModelPicker";
import { SettingGroup, SettingRow } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import { modelGroups } from "../../lib/modelPicker";
import { setDefaultProvider } from "../../lib/providers";
import { isTauriRuntime } from "../../overlay/events";
import { useDaemon } from "../../state/DaemonContext";
import { useProviders } from "../../state/useProviders";
import styles from "../SettingsModal.module.css";
import { ProvidersSection } from "../SettingsSections";
import { DetectedAccountsBlock } from "./DetectedAccountsBlock";
import type { CardDef } from "./types";

/** Default provider + model: a searchable dropdown grouped by provider. */
function DefaultModelRow() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const { dto, reload } = useProviders();
  const [override, setOverride] = useState<string | null>(null);
  const mock = client?.baseUrl === MOCK_BASE_URL;
  const canChange = isTauriRuntime() || mock;

  const current = useMemo(() => {
    const id = dto?.default_provider;
    const row = dto?.configured.find((p) => p.id === id);
    return id && row?.default_model ? `${id}/${row.default_model}` : "";
  }, [dto]);
  const groups = useMemo(() => modelGroups(dto, current), [dto, current]);

  const pick = async (ref: string) => {
    const i = ref.indexOf("/");
    const provider = ref.slice(0, i);
    const model = ref.slice(i + 1);
    if (mock) {
      setOverride(ref);
      return;
    }
    try {
      const r = await setDefaultProvider(provider, model);
      if (r.code !== 0) {
        throw new Error((r.stderr || r.stdout).trim());
      }
      await client?.reloadProviders().catch(() => undefined);
      await reload();
      toast.show(t("modelPicker.switchedDefault", { model: ref }), "success");
    } catch (e) {
      toast.show(e instanceof Error ? e.message : String(e), "danger");
    }
  };

  return (
    <SettingRow
      title={t("settingsModal.ai.defaultModel.title")}
      description={
        canChange
          ? t("settingsModal.ai.defaultModel.description")
          : t("settings.providers.nativeOnly")
      }
    >
      <ModelPicker
        groups={groups}
        value={override ?? current}
        disabled={!canChange || groups.length === 0}
        onPick={(ref) => void pick(ref)}
      />
    </SettingRow>
  );
}

/** Write-only API keys: typing stores the key through `POST /v1/secrets/{name}`. */
function ApiKeysBlock() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client } = useDaemon();
  const { dto, reload } = useProviders();
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [saved, setSaved] = useState<Record<string, boolean>>({});

  const rows = (dto?.catalog ?? []).filter(
    (c) => c.auth === "api_key_env" && c.key_env
  );

  const save = async (id: string, keyEnv: string) => {
    const value = (values[id] ?? "").trim();
    if (!client || !value) {
      return;
    }
    setBusy(id);
    try {
      await client.setSecret(keyEnv, value);
      setValues((v) => ({ ...v, [id]: "" }));
      setSaved((s) => ({ ...s, [id]: true }));
      await client.reloadProviders().catch(() => undefined);
      await reload();
      toast.show(t("settingsModal.ai.keys.saved", { provider: id }), "success");
    } catch (e) {
      toast.show(e instanceof Error ? e.message : String(e), "danger");
    } finally {
      setBusy(null);
    }
  };

  return (
    <SettingGroup>
      <SettingRow
        title={t("settingsModal.ai.keys.title")}
        description={t("settingsModal.ai.keys.description")}
      />
      {rows.length === 0 ? (
        <SettingRow title={t("settingsModal.ai.keys.none")} />
      ) : (
        rows.map((c) => {
          const hasKey =
            saved[c.id] ||
            dto?.configured.some((p) => p.id === c.id && p.has_key) === true;
          return (
            <SettingRow
              key={c.id}
              title={c.id}
              description={
                hasKey
                  ? t("settings.providers.keyStored")
                  : t("settings.providers.noKey")
              }
            >
              <input
                className={`${formStyles.input} ${styles.keyInput}`}
                type="password"
                autoComplete="off"
                aria-label={t("settingsModal.ai.keys.inputLabel", {
                  provider: c.id
                })}
                placeholder={
                  hasKey
                    ? t("settingsModal.ai.keys.replace")
                    : t("settingsModal.ai.keys.placeholder")
                }
                value={values[c.id] ?? ""}
                onChange={(e) =>
                  setValues((v) => ({ ...v, [c.id]: e.target.value }))
                }
              />
              <Button
                size="sm"
                variant="primary"
                disabled={busy === c.id || !(values[c.id] ?? "").trim()}
                onClick={() => void save(c.id, c.key_env ?? "")}
              >
                {t("common.save")}
              </Button>
            </SettingRow>
          );
        })
      )}
    </SettingGroup>
  );
}

/** Cards of the "AI" section. */
export function aiCards(t: TFunction): CardDef[] {
  return [
    {
      id: "defaultModel",
      section: "ai",
      kind: "row",
      title: t("settingsModal.ai.defaultModel.title"),
      description: t("settingsModal.ai.defaultModel.description"),
      keywords: "model provider varsayilan",
      node: <DefaultModelRow />
    },
    {
      id: "keys",
      section: "ai",
      kind: "block",
      title: t("settingsModal.ai.keys.title"),
      description: t("settingsModal.ai.keys.description"),
      keywords: "api key secret anahtar sifre",
      node: <ApiKeysBlock />
    },
    {
      id: "detectedAccounts",
      section: "ai",
      kind: "block",
      title: t("settingsModal.ai.detected.title"),
      description: t("settingsModal.ai.detected.description"),
      keywords: "cli claude codex cursor gemini opencode chatgpt account hesap algilanan",
      node: <DetectedAccountsBlock />
    },
    {
      id: "providers",
      section: "ai",
      kind: "block",
      title: t("settings.providers.title"),
      description: t("settings.providers.description"),
      keywords: "provider saglayici ollama openrouter",
      node: <ProvidersSection />
    }
  ];
}
