import { Monitor } from "lucide-react";
import type { TFunction } from "i18next";
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
import { EmailCard } from "./EmailCard";
import { WebSearchCard } from "./WebSearchCard";
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
  return (
    <div
      className={styles.targetGrid}
      role="radiogroup"
      aria-label={t("settingsModal.computer.target.title")}
    >
      {COMPUTER_TARGETS.map((target) => {
        const soon = !isComputerTargetAvailable(target);
        const active = parseComputerTarget(value) === target;
        return (
          <button
            key={target}
            type="button"
            role="radio"
            aria-checked={active}
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
      id: "permissions",
      section: "computer",
      kind: "block",
      title: t("settings.permissions.title"),
      description: t("settings.permissions.description"),
      keywords: "policy permission izin",
      node: <PermissionsSection />
    },
    {
      id: "email",
      section: "computer",
      kind: "block",
      title: t("settingsModal.email.title"),
      description: t("settingsModal.email.description"),
      keywords: "mail e-posta eposta imap smtp gmail outlook yahoo icloud mailbox posta kutusu",
      node: <EmailCard />
    },
    {
      id: "web-search",
      section: "computer",
      kind: "block",
      title: t("settingsModal.webSearch.title"),
      description: t("settingsModal.webSearch.description"),
      keywords: "web search searxng brave tavily duckduckgo bing arama internet",
      node: <WebSearchCard />
    }
  ];
}
