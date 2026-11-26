import { TrustedFolders } from "../../components/TrustedFolders";
import { Monitor } from "lucide-react";
import { SettingsCard } from "../../components/SettingsUi";
import type { TFunction } from "i18next";
import type { KeyboardEvent } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import {
  COMPUTER_TARGETS,
  isComputerTargetAvailable,
  parseComputerTarget,
  type ComputerTarget
} from "../../lib/settingsPrefs";
import { useLiveView } from "../../state/LiveViewContext";
import styles from "../SettingsModal.module.css";
import { PermissionsSection } from "../SettingsSections";
import { ComputerAccessCard } from "../../components/ComputerAccess";
import { CalendarCard } from "./CalendarCard";
import { WebSearchCard } from "./WebSearchCard";
import { WorkspaceStorageCard } from "./WorkspaceStorageCard";
import { AuditLogCard } from "./AuditLogCard";
import { AllowedGrantsCard } from "./AllowedGrantsCard";
import { PageReaderCard } from "./PageReaderCard";
import { SafetyLevelsCard } from "./SafetyLevelsCard";
import { SiteRulesCard } from "./SiteRulesCard";
import type { CardDef, SettingsBridge } from "./types";

function OpenLiveViewerRow() {
  const { t } = useTranslation();
  const { openViewer } = useLiveView();
  return (
    <div>
      <Button
        variant="secondary"
        size="sm"
        data-testid="computer-open-live-view"
        onClick={() => openViewer()}
      >
        <Monitor />
        <span>{t("liveView.expand")}</span>
      </Button>
    </div>
  );
}

/** The connected computer; alternate targets stay unavailable until daemon support exists. */
export function ComputerTargetBlock({
  value,
  onChange
}: {
  value: ComputerTarget;
  onChange: (v: ComputerTarget) => void;
}) {
  const { t } = useTranslation();
  const available = COMPUTER_TARGETS.filter((target) =>
    isComputerTargetAvailable(target)
  );
  const current = parseComputerTarget(value);
  const activeTarget = available.includes(current) ? current : available[0];
  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const step =
      e.key === "ArrowRight" || e.key === "ArrowDown"
        ? 1
        : e.key === "ArrowLeft" || e.key === "ArrowUp"
          ? -1
          : 0;
    if (!step || available.length === 0) {
      return;
    }
    e.preventDefault();
    const at = Math.max(0, available.indexOf(activeTarget));
    const next = available[(at + step + available.length) % available.length];
    onChange(next);
    e.currentTarget
      .querySelector<HTMLElement>(`[data-testid="computer-target-${next}"]`)
      ?.focus();
  };
  return (
    <div
      className={styles.targetGrid}
      role="radiogroup"
      aria-label={t("settingsModal.computer.target.title")}
      onKeyDown={onKeyDown}
    >
      {/* Modes that cannot be chosen yet are not listed: a "Soon" card is a promise the customer cannot use. */}
      {COMPUTER_TARGETS.filter(isComputerTargetAvailable).map((target) => {
        const active = current === target;
        return (
          <button
            key={target}
            type="button"
            role="radio"
            aria-checked={active}
            tabIndex={target === activeTarget ? 0 : -1}
            data-testid={`computer-target-${target}`}
            className={[styles.targetCard, active ? styles.targetActive : ""].filter(Boolean).join(" ")}
            onClick={() => onChange(target)}
          >
            <span className={styles.targetHead}>
              <span className={styles.targetTitle}>
                {t(`settingsModal.computer.targets.${target}.title`)}
              </span>
            </span>
            <span className={styles.targetDesc}>
              {t(`settingsModal.computer.targets.${target}.description`)}
            </span>
          </button>
        );
      })}
    </div>
  );
}

/** Cards of the "Computer" section. */
export function computerCards(b: SettingsBridge, t: TFunction): CardDef[] {
  return [
    {
      id: "safety-levels",
      section: "computer",
      kind: "block",
      title: t("settingsModal.safety.title"),
      description: t("settingsModal.safety.description"),
      keywords: "safety permissions ask approve allow safe careful balanced hands-off guvenlik izin onay guvenli dikkatli sorar sicherheit erlaubnis genehmigen vorsichtig",
      node: (
        <SettingsCard title={t("settingsModal.safety.title")} description={t("settingsModal.safety.description")}>
          <SafetyLevelsCard />
        </SettingsCard>
      )
    },
    {
      id: "page-reader",
      section: "computer",
      kind: "block",
      title: t("pageReader.title"),
      description: t("pageReader.description"),
      keywords: "web page reader browser prompt injection hidden text safe quarantine helper model sayfa okuyucu gizli metin guvenli tarayici seite leser versteckt sicher browser helfer modell lector pagina oculto seguro navegador",
      node: (
        <SettingsCard title={t("pageReader.title")} description={t("pageReader.description")}>
          <PageReaderCard />
        </SettingsCard>
      )
    },
    {
      id: "audit-log",
      section: "computer",
      kind: "block",
      title: t("audit.title"),
      description: t("audit.description"),
      keywords: "audit log history approvals activity export aktivite gunluk gecmis onay denetim protokoll verlauf genehmigung aktivitat aktivität",
      node: (
        <SettingsCard title={t("audit.title")} description={t("audit.description")}>
          <AuditLogCard />
        </SettingsCard>
      )
    },
    {
      id: "site-rules",
      section: "computer",
      kind: "block",
      title: t("settingsModal.siteRules.title"),
      description: t("settingsModal.siteRules.lead"),
      keywords: "site website program app rule always allow block ask domain kural site program izin engelle erlauben sperren regel webseite programm",
      node: <SettingsCard title={t("settingsModal.siteRules.title")}><SiteRulesCard /></SettingsCard>
    },
    {
      id: "target",
      section: "computer",
      kind: "block",
      title: t("settingsModal.computer.target.title"),
      description: t("settingsModal.computer.target.description"),
      keywords: "this pc browser remote host server computer off bilgisayar hedef tarayici uzak sunucu rechner ziel fern",
      node: (
        <SettingsCard
          title={t("settingsModal.computer.target.title")}
          description={t("settingsModal.computer.target.description")}
        >
          <div className={styles.blockStack}>
            <ComputerTargetBlock
              value={b.settings.computerTarget}
              onChange={(computerTarget) => b.update({ computerTarget })}
            />
            <OpenLiveViewerRow />
          </div>
        </SettingsCard>
      )
    },
    {
      id: "my-computer",
      section: "computer",
      kind: "block",
      title: t("computerAccess.title"),
      description: t("computerAccess.description"),
      keywords: "bots use computer screen control access server host bilgisayar erisim ekran sunucu botlarim izin zugriff bildschirm",
      node: (
        <SettingsCard title={t("computerAccess.title")} description={t("computerAccess.description")}>
          <ComputerAccessCard />
        </SettingsCard>
      )
    },
    {
      id: "trusted-folders",
      section: "computer",
      kind: "block",
      title: t("folderTrust.title"),
      description: t("folderTrust.description"),
      keywords: "trust folder directory repository path klasor guven dizin depo ordner vertrauen verzeichnis",
      node: (
        <SettingsCard title={t("folderTrust.title")}>
          <TrustedFolders />
        </SettingsCard>
      )
    },
    {
      id: "allowed-grants",
      section: "computer",
      kind: "block",
      title: t("settingsModal.safety.allowedGrants.title"),
      description: t("settingsModal.safety.allowedGrants.description"),
      keywords: "always allow site domain action revoke izin verilen siteler islemler kaldir her zaman izinli eylem site alan seite domain berechtigung widerrufen immer erlauben",
      node: <SettingsCard title={t("settingsModal.safety.allowedGrants.title")} description={t("settingsModal.safety.allowedGrants.description")}><AllowedGrantsCard /></SettingsCard>
    },
    {
      id: "permissions",
      section: "computer",
      kind: "block",
      title: t("settings.permissions.title"),
      description: t("settings.permissions.description"),
      keywords: "policy permission rules allow deny ask delete sil kural izin politika regeln erlaubnis löschen",
      node: <PermissionsSection />
    },
    {
      id: "web-search",
      section: "computer",
      kind: "block",
      title: t("settingsModal.webSearch.title"),
      description: t("settingsModal.webSearch.description"),
      keywords: "web search internet searxng brave tavily duckduckgo bing arama internet websuche suche",
      node: (
        <SettingsCard title={t("settingsModal.webSearch.title")} description={t("settingsModal.webSearch.description")}>
          <WebSearchCard />
        </SettingsCard>
      )
    },
    {
      id: "calendar",
      section: "computer",
      kind: "block",
      title: t("settingsModal.calendar.title"),
      description: t("settingsModal.calendar.description"),
      keywords: "calendar caldav event meeting invite google icloud fastmail nextcloud takvim etkinlik toplantı davet kalender termin",
      node: (
        <SettingsCard title={t("settingsModal.calendar.title")} description={t("settingsModal.calendar.description")}>
          <CalendarCard />
        </SettingsCard>
      )
    },
    {
      id: "bot-workspaces",
      section: "computer",
      kind: "block",
      title: t("workspace.cardTitle"),
      description: t("workspace.cardDescription"),
      keywords: "workspace folder disk storage quota limit cleanup downloads logs klasor alan depolama",
      node: (
        <SettingsCard
          title={t("workspace.cardTitle")}
          description={t("workspace.cardDescription")}
        >
          <WorkspaceStorageCard />
        </SettingsCard>
      )
    }
  ];
}
