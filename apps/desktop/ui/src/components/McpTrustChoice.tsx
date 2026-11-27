import { useTranslation } from "react-i18next";
import { openExternal } from "../lib/externalLink";
import { LearnMore } from "./LearnMore";
import formStyles from "./Form.module.css";
import styles from "./McpTrustChoice.module.css";

/** Where the guide explains the choice (the MCP servers page). */
export const TRUST_DOCS_URL =
  "https://github.com/emreocell/Praxi-Bot/blob/main/docs/guide/mcp-servers.md";

/**
 * "Do you trust this server to describe its own tools honestly?"  The safe
 * answer is the default: No, every action asks.  Yes lets only the tools the
 * server labels read-only run without asking; it never covers anything that
 * changes or deletes data.
 */
export function McpTrustChoice({
  value,
  onChange,
  id,
  disabled = false,
  compact = false
}: {
  value: boolean;
  onChange: (trusted: boolean) => void;
  /** Distinguishes several choices on one screen (names, test ids). */
  id: string;
  disabled?: boolean;
  /** The explanation sits behind a "?" next to the question instead of under the answers. */
  compact?: boolean;
}) {
  const { t } = useTranslation();
  const option = (trusted: boolean, label: string) => (
    <label className={styles.option}>
      <input
        type="radio"
        name={`mcp-trust-${id}`}
        checked={value === trusted}
        disabled={disabled}
        data-testid={`mcp-trust-${id}-${trusted ? "yes" : "no"}`}
        onChange={() => onChange(trusted)}
      />
      <span>{label}</span>
    </label>
  );
  return (
    <fieldset className={`${formStyles.field} ${styles.options}`} data-testid={`mcp-trust-${id}`}>
      <legend className={`${formStyles.label} ${styles.legend}`}>
        {t("apps.trust.question")}
        {compact ? <LearnMore term="mcpTrust" /> : null}
      </legend>
      {option(false, t("apps.trust.no"))}
      {option(true, t("apps.trust.yes"))}
      {compact ? null : (
      <p className={formStyles.hint}>
        {t("apps.trust.explain")}{" "}
        <button type="button" className={styles.link} onClick={() => void openExternal(TRUST_DOCS_URL)}>
          {t("apps.trust.learn")}
        </button>
      </p>
      )}
    </fieldset>
  );
}
