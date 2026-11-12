import { TriangleAlert } from "lucide-react";
import { useId } from "react";
import { useTranslation } from "react-i18next";
import type { Bot } from "../api/types";
import { type BotScope, toggleBotInScope } from "../lib/mcpApps";
import formStyles from "./Form.module.css";
import { Segmented } from "./SettingsUi";
import styles from "./BotAccessPicker.module.css";

/**
 * "All bots / Only these bots" for one MCP server.  Writes bot storage UUIDs;
 * shows names.  Choosing "only these bots" starts from nobody, and an emptied
 * list is kept (and warned about), never silently widened to everyone.
 */
export function BotAccessPicker({
  scope,
  bots,
  onChange
}: {
  scope: BotScope;
  bots: Pick<Bot, "id" | "name">[];
  onChange: (scope: BotScope) => void;
}) {
  const { t } = useTranslation();
  const uid = useId();
  return (
    <div className={styles.root} data-testid="bot-access-picker">
      <Segmented
        label={t("apps.access.label")}
        value={scope.mode}
        onChange={(mode) =>
          onChange(
            mode === "all"
              ? { mode: "all" }
              : { mode: "some", ids: scope.mode === "some" ? scope.ids : [] }
          )
        }
        options={[
          { value: "all", label: t("apps.access.all") },
          { value: "some", label: t("apps.access.some") }
        ]}
      />
      {scope.mode === "some" ? (
        <>
          {bots.length === 0 ? (
            <p className={formStyles.hint}>{t("apps.access.noBotsYet")}</p>
          ) : (
            <ul className={styles.list}>
              {bots.map((b) => (
                <li key={b.id}>
                  <label className={styles.item}>
                    <input
                      id={`${uid}-${b.id}`}
                      type="checkbox"
                      className={styles.check}
                      checked={scope.ids.includes(b.id)}
                      onChange={(e) =>
                        onChange(toggleBotInScope(scope, b.id, e.target.checked))
                      }
                    />
                    <span className={styles.name}>{b.name}</span>
                  </label>
                </li>
              ))}
            </ul>
          )}
          {scope.ids.length === 0 ? (
            <p className={styles.warn} role="status">
              <TriangleAlert aria-hidden="true" />
              {t("apps.access.nobody")}
            </p>
          ) : null}
        </>
      ) : null}
    </div>
  );
}
