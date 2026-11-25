import type { TFunction } from "i18next";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { MOCK_BASE_URL } from "../../api/mockMode";
import { ModelPicker } from "../../components/ModelPicker";
import { SettingGroup, SettingRow, SettingsCard } from "../../components/SettingsUi";
import { useToast } from "../../components/Toast";
import { modelGroups } from "../../lib/modelPicker";
import { defaultModelRef } from "../../lib/providerView";
import { canManageProviders } from "../../lib/providerConnection";
import { isTauriRuntime } from "../../overlay/events";
import { useDaemon } from "../../state/DaemonContext";
import { useProviders } from "../../state/useProviders";
import { ProvidersSection } from "../SettingsSections";
import { ApiKeyPaste } from "./ApiKeyPaste";
import { LocalInstallCard } from "../../components/LocalInstallCard";
import { LocalModelsAdvice } from "../../components/LocalModelsAdvice";
import { EnginesCard } from "./HomeCards";
import { HelpersCard } from "./HelpersCard";
import { KnowledgeCard } from "./KnowledgeCard";
import { MemoryCard } from "./MemoryCard";
import type { CardDef } from "./types";
import { humanLine } from "../../lib/humanError";
import { useBotsHome } from "../../state/useBotsHome";
import { ThisComputerOnly } from "../../components/ThisComputerOnly";
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
      if (home.target !== "host") {
        // A server rebuilds its own providers after an administrator change; peers may not ask.
        await client?.reloadProviders().catch(() => {});
      }
      await reload();
      toast.show(t("modelPicker.switchedDefault", { model: ref }), "success");
    } catch (e) {
      toast.show(humanLine(e), "danger");
    }
  };

  return (
    <SettingGroup>
      <SettingRow
        title={t("settingsModal.ai.defaultModel.title")}
        description={
          canChange
            ? t("settingsModal.ai.defaultModel.description")
            : t("settings.providers.localDaemonOnly")
        }
      >
        <HomeHint />
        <ModelPicker
          groups={groups}
          value={override ?? current}
          disabled={!canChange}
          onPick={(ref) => void pick(ref)}
        />
      </SettingRow>
    </SettingGroup>
  );
}

/** Cards of the "AI models" section: default, keys, detected tools, memory, knowledge, local & advanced. */
export function aiCards(t: TFunction): CardDef[] {
  return [
    {
      id: "defaultModel",
      section: "ai",
      kind: "block",
      title: t("settingsModal.ai.defaultModel.title"),
      description: t("settingsModal.ai.defaultModel.description"),
      keywords: "model default ai brain varsayilan standartmodell modell",
      node: <DefaultModelRow />
    },
    {
      id: "keys",
      section: "ai",
      kind: "block",
      title: t("settingsModal.ai.keys.title"),
      description: t("settingsModal.ai.keys.description"),
      keywords: "api key secret password anahtar sifre erisim passwort schluessel schlüssel zugang openai anthropic gemini openrouter",
      node: <ApiKeyPaste />
    },
    {
      id: "detectedAccounts",
      section: "ai",
      kind: "block",
      title: t("settingsModal.ai.detected.title"),
      description: t("settingsModal.ai.detected.description"),
      keywords: "cli claude codex cursor gemini opencode chatgpt account tool hesap arac algilanan bulunan werkzeuge",
      node: <EnginesCard />
    },
    {
      id: "memory",
      section: "ai",
      kind: "block",
      title: t("memory.title"),
      description: t("memory.description"),
      keywords: "memory recall forget undo remember delete hafiza hatirla unut sil geri al gedächtnis merken vergessen löschen",
      node: <MemoryCard />
    },
    {
      id: "knowledge",
      section: "ai",
      kind: "block",
      title: t("knowledge.title"),
      description: t("knowledge.description"),
      keywords: "rag memory knowledge files notes kaynak hafiza bilgi dosya notlar brain beyin wissen dateien notizen",
      node: <KnowledgeCard />
    },
    {
      id: "localModels",
      section: "ai",
      kind: "block",
      title: t("localModels.cardTitle"),
      description: t("localModels.cardDescription"),
      keywords: "ollama lmstudio local model hardware gpu ram yerel donanim lokal modell hardware",
      node: (
        <SettingsCard title={t("localModels.cardTitle")} description={t("localModels.cardDescription")}>
          <ThisComputerOnly>
            <LocalInstallCard />
            <LocalModelsAdvice />
          </ThisComputerOnly>
        </SettingsCard>
      )
    },
    {
      id: "helpers",
      section: "ai",
      kind: "block",
      title: t("helpers.title"),
      description: t("helpers.description"),
      keywords: "embedding embeddings gemini openai ollama decision helper yardimci karar arama helfer entscheidung bedeutung suche",
      node: <HelpersCard />
    },
    {
      id: "providers",
      section: "ai",
      kind: "block",
      title: t("settings.providers.title"),
      description: t("settings.providers.description"),
      keywords: "provider server custom compatible ollama openrouter saglayici sunucu ozel gelismis advanced api key anahtar schluessel anbieter benutzerdefiniert",
      node: <ProvidersSection />
    }
  ];
}
