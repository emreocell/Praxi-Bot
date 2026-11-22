import type { TFunction } from "i18next";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { MOCK_BASE_URL } from "../../api/mockMode";
import { ModelPicker } from "../../components/ModelPicker";
import { useToast } from "../../components/Toast";
import { modelGroups } from "../../lib/modelPicker";
import { defaultModelRef } from "../../lib/providerView";
import { canManageProviders } from "../../lib/providerConnection";
import { isTauriRuntime } from "../../overlay/events";
import { useDaemon } from "../../state/DaemonContext";
import { useProviders } from "../../state/useProviders";
import pageStyles from "./AiPage.module.css";
import { ProvidersSection } from "../SettingsSections";
import { ApiKeyPaste } from "./ApiKeyPaste";
import { LocalInstallCard } from "../../components/LocalInstallCard";
import { LocalModelsAdvice } from "../../components/LocalModelsAdvice";
import { EnginesCard } from "./HomeCards";
import { KnowledgeCard } from "./KnowledgeCard";
import { MemoryCard } from "./MemoryCard";
import type { CardDef } from "./types";
import { humanLine } from "../../lib/humanError";
import { useBotsHome } from "../../state/useBotsHome";
import { HomeHint } from "../../components/HomeHint";

/** Default provider + model: a searchable dropdown grouped by provider. */
function DefaultModelRow() {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, conn } = useDaemon();
  const { dto, reload } = useProviders();
  const [override, setOverride] = useState<string | null>(null);
  const mock = client?.baseUrl === MOCK_BASE_URL;
  const providerTarget = { connection: conn, baseUrl: client?.baseUrl };
  const home = useBotsHome();
  const canChange = (isTauriRuntime() && canManageProviders(providerTarget)) || mock || home.target === "host";

  const current = useMemo(() => defaultModelRef(dto), [dto]);
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
      await home.setDefaultModel(provider, model);
      await client?.reloadProviders().catch(() => {});
      await reload();
      toast.show(t("modelPicker.switchedDefault", { model: ref }), "success");
    } catch (e) {
      toast.show(humanLine(e), "danger");
    }
  };

  return (
    <section className={pageStyles.sec} aria-labelledby="ai-default-h">
      <div className={pageStyles.head}>
        <h3 id="ai-default-h">{t("settingsModal.ai.defaultModel.heading")}</h3>
        <HomeHint />
      </div>
      <div className={`${pageStyles.card} ${pageStyles.item}`}>
        <div className={pageStyles.text}>
          <span>{t("settingsModal.ai.defaultModel.title")}</span>
          <span className={pageStyles.sub}>
            {canChange
              ? t("settingsModal.ai.defaultModel.description")
              : t("settings.providers.localDaemonOnly")}
          </span>
        </div>
        <ModelPicker
          groups={groups}
          value={override ?? current}
          disabled={!canChange || groups.length === 0}
          onPick={(ref) => void pick(ref)}
        />
      </div>
    </section>
  );
}

/** Provider forms for servers the catalog does not cover; closed by default. */
function AdvancedProviders() {
  const { t } = useTranslation();
  return (
    <details className={pageStyles.advanced} id="ai-advanced-providers">
      <summary>{t("settingsModal.ai.advanced")}</summary>
      <ProvidersSection />
    </details>
  );
}

/** Cards of the "AI" section: found on this computer, API key, default, advanced. */
export function aiCards(t: TFunction): CardDef[] {
  return [
    { id: "knowledge", section: "ai", kind: "block", title: t("knowledge.title"), description: t("knowledge.description"), keywords: "rag memory knowledge kaynak hafiza bilgi brain beyin", node: <KnowledgeCard /> },
    { id: "memory", section: "ai", kind: "block", title: t("memory.title"), description: t("memory.description"), keywords: "memory recall forget undo remember hafiza hatirla unut geri al gedächtnis", node: <MemoryCard /> },
    {
      id: "detectedAccounts",
      section: "ai",
      kind: "block",
      title: t("settingsModal.ai.detected.title"),
      description: t("settingsModal.ai.detected.description"),
      keywords: "cli claude codex cursor gemini opencode chatgpt account hesap algilanan bulunan",
      node: <EnginesCard />
    },
    {
      id: "localModels",
      section: "ai",
      kind: "block",
      title: t("localModels.cardTitle"),
      description: t("localModels.cardDescription"),
      keywords: "ollama lmstudio local model hardware gpu ram yerel donanim lokal modell hardware",
      node: (
        <>
          <LocalInstallCard />
          <LocalModelsAdvice />
        </>
      )
    },
    {
      id: "keys",
      section: "ai",
      kind: "block",
      title: t("settingsModal.ai.keys.title"),
      description: t("settingsModal.ai.keys.description"),
      keywords: "api key secret anahtar sifre openai anthropic gemini openrouter",
      node: <ApiKeyPaste />
    },
    {
      id: "defaultModel",
      section: "ai",
      kind: "block",
      title: t("settingsModal.ai.defaultModel.title"),
      description: t("settingsModal.ai.defaultModel.description"),
      keywords: "model provider varsayilan",
      node: <DefaultModelRow />
    },
    {
      id: "providers",
      section: "ai",
      kind: "block",
      title: t("settings.providers.title"),
      description: t("settings.providers.description"),
      keywords: "provider saglayici ollama openrouter gelismis advanced compatible api key anahtar",
      node: <AdvancedProviders />
    }
  ];
}
