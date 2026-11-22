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
import { WebSearchCard } from "./WebSearchCard";
import { SafetyLevelsCard } from "./SafetyLevelsCard";
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
      {COMPUTER_TARGETS.map((target) => {
        const soon = !isComputerTargetAvailable(target);
        const active = current === target;
        return (
          <button
            key={target}
            type="button"
            role="radio"
            aria-checked={active}
            tabIndex={!soon && target === activeTarget ? 0 : -1}
            disabled={soon}
            data-testid={`computer-target-${target}`}
            className={[
              styles.targetCard,
              active ? styles.targetActive : "",
              soon ? styles.targetSoon : ""
            ]
              .filter(Boolean)
              .join(" ")}
            onClick={() => onChange(target)}
          >
            <span className={styles.targetHead}>
              <span className={styles.targetTitle}>
                {t(`settingsModal.computer.targets.${target}.title`)}
              </span>
              {soon ? (
                <span className={styles.badge}>
                  {t("settingsModal.computer.soon")}
                </span>
              ) : null}
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
      keywords: "permissions ask approve allow safe careful balanced hands-off izin onay guvenli dikkatli sorar erlauben sicher vorsichtig",
      node: <SettingsCard title={t("settingsModal.safety.title")}><SafetyLevelsCard /></SettingsCard>
    },
    {
      id: "target",
      section: "computer",
      kind: "block",
      title: t("settingsModal.computer.target.title"),
      description: t("settingsModal.computer.target.description"),
      keywords: "this pc browser remote host server off bilgisayar tarayici uzak",
      node: (
        <div className={styles.blockStack}>
          <p className={styles.blockIntro}>
            {t("settingsModal.computer.target.description")}
          </p>
          <ComputerTargetBlock
            value={b.settings.computerTarget}
            onChange={(computerTarget) => b.update({ computerTarget })}
          />
          <OpenLiveViewerRow />
        </div>
      )
    },
    {
      id: "my-computer",
      section: "computer",
      kind: "block",
      title: t("computerAccess.title"),
      description: t("computerAccess.description"),
      keywords: "bots use computer server host bilgisayar sunucu botlarim izin",
      node: <SettingsCard title={t("computerAccess.title")}><ComputerAccessCard /></SettingsCard>
    },
    {
      id: "trusted-folders",
      section: "computer",
      kind: "block",
      title: t("folderTrust.title"),
      description: t("folderTrust.description"),
      keywords: "trust folder repository klasor guven depo",
      node: <SettingsCard title={t("folderTrust.title")}><TrustedFolders /></SettingsCard>
    },
    {
      id: "permissions",
      section: "computer",
      kind: "block",
      title: t("settings.permissions.title"),
      description: t("settings.permissions.description"),
      keywords: "policy permission izin",
      node: <PermissionsSection />
    },
    {
      id: "web-search",
      section: "computer",
      kind: "block",
      title: t("settingsModal.webSearch.title"),
      description: t("settingsModal.webSearch.description"),
      keywords: "web search searxng brave tavily duckduckgo bing arama internet",
      node: <SettingsCard title={t("settingsModal.webSearch.title")} description={t("settingsModal.webSearch.description")}><WebSearchCard /></SettingsCard>
    }
  ];
}
