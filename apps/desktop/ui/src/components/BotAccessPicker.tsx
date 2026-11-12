import { TriangleAlert } from "lucide-react";
import { BotAvatar } from "ui";
import { useId } from "react";
import { useTranslation } from "react-i18next";
import type { Bot } from "../api/types";
import { botView } from "../lib/botProfile";
import { type BotScope, toggleBotInScope } from "../lib/mcpApps";
import formStyles from "./Form.module.css";
import { Segmented } from "./SettingsUi";
import styles from "./BotAccessPicker.module.css";

/** Wording of the picker; the defaults are the MCP "app" texts. */
export interface BotAccessTexts {
  label: string;
  all: string;
  some: string;
  nobody: string;
}

/**
 * "All bots / Only these bots" for one MCP server.  Writes bot storage UUIDs;
 * shows names.  Choosing "only these bots" starts from nobody, and an emptied
 * list is kept (and warned about), never silently widened to everyone.
 *
 * With `single` the choice is exactly one bot (radio buttons), for things that
 * belong to one bot such as a mailbox.
 */
export function BotAccessPicker({
  scope,
  bots,
  onChange,
  single = false,
  texts,
  allWarning
}: {
  scope: BotScope;
  bots: (Pick<Bot, "id" | "name"> & Partial<Pick<Bot, "config">>)[];
  onChange: (scope: BotScope) => void;
  single?: boolean;
  texts?: Partial<BotAccessTexts>;
  /** One line shown while "all bots" is chosen. */
  allWarning?: string;
}) {
  const { t } = useTranslation();
  const uid = useId();
  const words: BotAccessTexts = {
    label: texts?.label ?? t("apps.access.label"),
    all: texts?.all ?? t("apps.access.all"),
    some: texts?.some ?? t("apps.access.some"),
    nobody: texts?.nobody ?? t("apps.access.nobody")
  };
  return (
    <div className={styles.root} data-testid="bot-access-picker">
      <Segmented
        label={words.label}
        value={scope.mode}
        onChange={(mode) =>
          onChange(
            mode === "all"
              ? { mode: "all" }
              : { mode: "some", ids: scope.mode === "some" ? scope.ids : [] }
          )
        }
        options={[
          { value: "all", label: words.all },
          { value: "some", label: words.some }
        ]}
      />
      {scope.mode === "all" && allWarning ? (
        <p className={styles.warn} role="status">
          <TriangleAlert aria-hidden="true" />
          {allWarning}
        </p>
      ) : null}
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
                      type={single ? "radio" : "checkbox"}
                      name={single ? `${uid}-bot` : undefined}
                      className={styles.check}
                      checked={scope.ids.includes(b.id)}
                      onChange={(e) =>
                        onChange(
                          single
                            ? { mode: "some", ids: [b.id] }
                            : toggleBotInScope(scope, b.id, e.target.checked)
                        )
                      }
                    />
                    <BotAvatar bot={b.config === undefined ? { id: b.id, name: b.name } : botView(b as Bot)} size="sm" decorative />
                    <span className={styles.name}>{b.name}</span>
                  </label>
                </li>
              ))}
            </ul>
          )}
          {scope.ids.length === 0 ? (
            <p className={styles.warn} role="status">
              <TriangleAlert aria-hidden="true" />
              {words.nobody}
            </p>
          ) : null}
        </>
      ) : null}
    </div>
  );
}
