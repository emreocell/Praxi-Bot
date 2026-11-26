import { Plus } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { hasRealModel } from "../demo/demoEngine";
import { useProviders } from "../state/useProviders";
import { SetupChecklist } from "./SetupChecklist";
import styles from "./SetupChecklist.module.css";

/**
 * The home screen while this computer has no bot yet: two numbered steps (connect an AI, create the
 * first bot) with one obvious button, instead of three equal buttons and a paragraph.
 */
export function HomeFirstSteps({
  onConnectModel,
  onNewBot,
  onRefresh
}: {
  onConnectModel: () => void;
  onNewBot: () => void;
  onRefresh: () => void;
}) {
  const { t } = useTranslation();
  const { dto } = useProviders();
  // Still loading counts as "not connected yet": the step then simply offers the wizard.
  const modelDone = dto !== null && hasRealModel(dto);
  return (
    <div className={styles.home} data-testid="home-first-steps">
      <SetupChecklist
        label={t("main.welcomeTitle")}
        steps={[
          {
            id: "model",
            title: t("setup.home.model"),
            hint: t("setup.home.modelHint"),
            done: modelDone,
            action: { label: t("setup.home.modelAction"), testId: "home-step-model", onClick: onConnectModel }
          },
          {
            id: "bot",
            title: t("setup.home.bot"),
            hint: t("setup.home.botHint"),
            done: false,
            action: { label: t("sidebar.newBot"), icon: <Plus aria-hidden="true" />, testId: "home-step-bot", onClick: onNewBot }
          },
          { id: "chat", title: t("setup.home.chat"), hint: "", done: false }
        ]}
      />
      <Button variant="ghost" size="sm" onClick={onRefresh}>
        {t("setup.home.missingBots")}
      </Button>
    </div>
  );
}
