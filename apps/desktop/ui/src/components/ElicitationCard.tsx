import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { ApiError } from "../api/client";
import type { ElicitationField } from "../api/types";
import { escapeInvisible } from "../lib/approvalPlain";
import { readElicitation } from "../lib/elicitationForm";
import { formatCountdown, secondsLeft } from "../lib/secretRequestForm";
import { useDaemon } from "../state/DaemonContext";
import type { ElicitationItem } from "../state/timeline";
import formStyles from "./Form.module.css";
import styles from "./SecretRequestCard.module.css";

function Field({ field, index, id, disabled }: {
  field: ElicitationField;
  index: number;
  id: string;
  disabled: boolean;
}) {
  const { t } = useTranslation();
  const props = {
    id, name: String(index), disabled,
    required: field.required,
    autoComplete: "off",
    "aria-describedby": field.description || field.format === "date_time" ? `${id}-hint` : undefined
  };
  const select = field.kind === "enum" || field.kind === "boolean";
  const number = field.kind === "number" || field.kind === "integer";
  const inputType = number ? "number" : field.format === "email" ? "email" : field.format === "date" ? "date" : "text";
  if (field.blocked) {
    return (
      <p className={styles.warn}>
        <bdi>{escapeInvisible(field.label)}</bdi>{" "}{t("elicitation.blockedField")}
      </p>
    );
  }
  return (
    <div className={formStyles.field}>
      <label className={formStyles.label} htmlFor={id}>
        <bdi>{escapeInvisible(field.label)}</bdi>{field.required ? " *" : null}
      </label>
      {select ? (
        <select {...props} className={formStyles.input} defaultValue="">
          <option value="">{t("elicitation.select")}</option>
          {field.kind === "boolean" ? (
            <>
              <option value="yes">{t("elicitation.yes")}</option>
              <option value="no">{t("elicitation.no")}</option>
            </>
          ) : field.options.map((option, n) => (
            <option key={n} value={String(n)}>{escapeInvisible(option.label)}</option>
          ))}
        </select>
      ) : (
        <input
          {...props}
          className={formStyles.input}
          type={inputType}
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          step={number ? field.kind === "integer" ? 1 : "any" : undefined}
          min={field.minimum ?? undefined}
          max={field.maximum ?? undefined}
          minLength={field.min_length ?? undefined}
          maxLength={Math.min(1024, field.max_length ?? 1024)}
        />
      )}
      {props["aria-describedby"] ? (
        <small id={`${id}-hint`}>
          {field.description ? <bdi>{escapeInvisible(field.description)}</bdi> : null}
          {field.format === "date_time" ? ` ${t("elicitation.dateTimeHint")}` : null}
        </small>
      ) : null}
    </div>
  );
}

/** The only copies of edits live in the form inputs until a submit. */
export function ElicitationCard({ item }: { item: ElicitationItem }) {
  const { t } = useTranslation();
  const { client, refreshRuns } = useDaemon();
  const id = useId();
  const [now, setNow] = useState(() => Date.now());
  const [busy, setBusy] = useState(false);
  const [sent, setSent] = useState(false);
  const [error, setError] = useState<"invalid" | "failed" | null>(null);
  const form = useRef<HTMLFormElement>(null);
  const left = secondsLeft(item.expiresMs, now);
  const closed = item.closed || sent || left === 0;
  const blocked = item.fields.some((field) => field.blocked && field.required);

  useEffect(() => {
    if (closed) return;
    const tick = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(tick);
  }, [closed]);

  const decide = async (action: "accept" | "decline" | "cancel") => {
    if (closed || busy || (action === "accept" && blocked)) return;
    setError(null);
    setBusy(true);
    try {
      if (!client) throw new Error();
      if (action === "accept" && form.current) {
        const content = readElicitation(item.fields, new FormData(form.current));
        await client.decideElicitation(item.callId, content);
      } else if (action === "decline") {
        await client.decideElicitation(item.callId);
      } else if (action === "cancel") {
        await client.cancelElicitation(item.callId);
      }
      form.current?.reset();
      setSent(true);
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) {
        form.current?.reset();
        setSent(true);
        void refreshRuns();
      } else {
        setError(err instanceof ApiError && err.status === 400 ? "invalid" : "failed");
      }
    } finally {
      setBusy(false);
    }
  };
  const submit = (event: FormEvent) => {
    event.preventDefault();
    void decide("accept");
  };

  return (
    <section className={styles.card} aria-labelledby={`${id}-title`} data-testid="elicitation-card">
      <div className={styles.head}>
        <h2 className={styles.title} id={`${id}-title`}>
          {t("elicitation.title", { server: escapeInvisible(item.server) })}
        </h2>
        {!closed ? (
          <span className={styles.countdown} role="timer" aria-label={t("elicitation.expiresIn")}>
            {formatCountdown(left)}
          </span>
        ) : null}
      </div>
      <p className={styles.purpose} dir="auto">{escapeInvisible(item.message)}</p>
      {closed ? (
        <p className={styles.status} role="status">
          {left === 0 && !sent && !item.closed ? t("elicitation.expired") : t("elicitation.closed")}
        </p>
      ) : (
        <>
          <p className={styles.note}>{t("elicitation.warning")}</p>
          <form ref={form} className={styles.form} autoComplete="off" onSubmit={submit}>
            {item.fields.map((field, index) => (
              <Field key={index} id={`${id}-field-${index}`} field={field} index={index} disabled={busy} />
            ))}
            {error ? (
              <p className={styles.warn} role="alert">
                {error === "invalid" ? t("elicitation.invalid") : t("elicitation.failed")}
              </p>
            ) : null}
            <div className={styles.actions}>
              <Button type="submit" disabled={busy || blocked}>{t("elicitation.submit")}</Button>
              <Button type="button" variant="secondary" disabled={busy} onClick={() => void decide("decline")}>
                {t("elicitation.decline")}
              </Button>
              <Button type="button" variant="ghost" disabled={busy} onClick={() => void decide("cancel")}>
                {t("common.cancel")}
              </Button>
            </div>
          </form>
        </>
      )}
    </section>
  );
}
