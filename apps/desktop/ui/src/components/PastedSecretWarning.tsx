import { useId, useState } from "react";
import { ShieldAlert } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { humanLine } from "../lib/humanError";
import { isValidSecretName, suggestedName } from "../lib/secretDetect";
import formStyles from "./Form.module.css";
import styles from "./PastedSecretWarning.module.css";

export interface PastedSecretWarningProps {
  /** Kinds of what was found (`openai_style_key` ...), never the values. */
  kinds: readonly string[];
  onSendAnyway: () => void;
  onRemove: () => void;
  /** Put the value(s) in the vault under `name` and swap them for placeholders in the draft. */
  onStore: (name: string) => Promise<void>;
  onDismiss: () => void;
}

/**
 * Shown instead of sending when the draft looks like it holds a credential. It names the kind
 * and offers three ways out; it never shows, copies or keeps the value (the draft stays in the
 * composer exactly as typed, and the parent reads the value only when "store" is pressed).
 */
export function PastedSecretWarning({ kinds, onSendAnyway, onRemove, onStore, onDismiss }: PastedSecretWarningProps) {
  const { t } = useTranslation();
  const uid = useId();
  const [naming, setNaming] = useState(false);
  const [name, setName] = useState(() => suggestedName(kinds[0] ?? "secret"));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const kindList = [...new Set(kinds)].map((k) => t(`pastedSecret.kind.${k}`, { defaultValue: k })).join(", ");
  const valid = isValidSecretName(name);

  const store = async () => {
    if (!valid || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onStore(name);
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className={styles.card} role="alertdialog" aria-labelledby={`${uid}-t`} aria-describedby={`${uid}-d`} data-testid="pasted-secret-warning">
      <h3 className={styles.title} id={`${uid}-t`}>
        <ShieldAlert size={16} aria-hidden="true" />
        {t("pastedSecret.title")}
      </h3>
      <p className={styles.text} id={`${uid}-d`}>
        {t("pastedSecret.body", { kinds: kindList })}
      </p>
      {naming ? (
        <div className={formStyles.field}>
          <label className={formStyles.label} htmlFor={`${uid}-n`}>{t("pastedSecret.nameLabel")}</label>
          <input
            id={`${uid}-n`}
            className={formStyles.input}
            value={name}
            onChange={(e) => setName(e.target.value)}
            aria-invalid={!valid}
            autoComplete="off"
            spellCheck={false}
            disabled={busy}
          />
          <p className={formStyles.hint}>{t("pastedSecret.nameHint")}</p>
        </div>
      ) : null}
      <div className={styles.actions}>
        {naming ? (
          <Button size="sm" variant="primary" disabled={!valid || busy} onClick={() => void store()}>
            {t("pastedSecret.storeConfirm")}
          </Button>
        ) : (
          <Button size="sm" variant="primary" onClick={() => setNaming(true)}>
            {t("pastedSecret.store")}
          </Button>
        )}
        <Button size="sm" variant="secondary" disabled={busy} onClick={onRemove}>
          {t("pastedSecret.remove")}
        </Button>
        <Button size="sm" variant="secondary" disabled={busy} onClick={onSendAnyway}>
          {t("pastedSecret.sendAnyway")}
        </Button>
        <Button size="sm" variant="ghost" disabled={busy} onClick={onDismiss}>
          {t("pastedSecret.back")}
        </Button>
      </div>
      {error ? <p className={formStyles.error} role="alert">{error}</p> : null}
    </div>
  );
}
