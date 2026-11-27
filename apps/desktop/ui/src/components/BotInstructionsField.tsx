import { Plus } from "lucide-react";
import { useId } from "react";
import { useTranslation } from "react-i18next";
import { appendInstruction, INSTRUCTION_EXAMPLES } from "../lib/botInstructions";
import styles from "./BotEditorFields.module.css";
import { dialogStyles, Field } from "./DialogParts";

/**
 * "What it should do": the bot's standing instruction, with ready examples to start from
 * instead of a blank box.  An example fills an empty box, or is added below what is there.
 */
export function BotInstructionsField({
  value,
  onChange
}: {
  value: string;
  onChange: (value: string) => void;
}) {
  const { t } = useTranslation();
  const uid = useId();
  return (
    <>
      <Field label={t("botEdit.instructions.label")} htmlFor={`${uid}-text`} hint={t("botEdit.instructions.hint")}>
        <textarea
          id={`${uid}-text`}
          className={`${dialogStyles.textarea} ${styles.instructions}`}
          rows={7}
          value={value}
          placeholder={t("botEdit.instructions.placeholder")}
          data-testid="bot-edit-instructions"
          onChange={(e) => onChange(e.target.value)}
        />
      </Field>
      <div className={styles.examples} role="group" aria-labelledby={`${uid}-examples`}>
        <span id={`${uid}-examples`} className={styles.examplesLabel}>
          {value.trim() ? t("botEdit.instructions.addExample") : t("botEdit.instructions.startFrom")}
        </span>
        {INSTRUCTION_EXAMPLES.map((id) => (
          <button
            key={id}
            type="button"
            className={styles.exampleChip}
            title={t(`botEdit.instructions.examples.${id}.text`)}
            data-testid={`bot-edit-example-${id}`}
            onClick={() => onChange(appendInstruction(value, t(`botEdit.instructions.examples.${id}.text`)))}
          >
            <Plus aria-hidden="true" />
            {t(`botEdit.instructions.examples.${id}.label`)}
          </button>
        ))}
      </div>
    </>
  );
}
