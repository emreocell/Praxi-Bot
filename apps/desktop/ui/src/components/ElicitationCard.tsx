import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { ClipboardList, Server, ShieldAlert } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { ApiError } from "../api/client";
import type { AnswerElicitation, ElicitationField } from "../api/types";
import type { ElicitationItem } from "../state/timeline";
import { escapeInvisible } from "../lib/approvalPlain";
import { humanLine } from "../lib/humanError";
import { NoClientError, formatCountdown, secondsLeft } from "../lib/secretRequestForm";
import {
  buildContent,
  defaultChecked,
  defaultText,
  hasUnsatisfiableField,
  inputType,
  isElicitationGone,
  type RawValue
} from "../lib/elicitationForm";
import { splitExternalLinks } from "../lib/externalLinks";
import formStyles from "./Form.module.css";
import styles from "./ElicitationCard.module.css";

export interface ElicitationCardProps {
  item: ElicitationItem;
  onAnswer: (body: AnswerElicitation) => Promise<void>;
  onCancel: () => Promise<void>;
}

/**
 * Untrusted server text as plain text nodes. A web address in it is shown as inert monospace
 * text with an "external link" tag: never an anchor, never opened by Praxi.
 */
function ServerText({ text }: { text: string }) {
  const { t } = useTranslation();
  return (
    <>
      {splitExternalLinks(text).map((seg, i) =>
        seg.kind === "url" ? (
          <span key={i} className={styles.link} data-external-link="true" title={t("elicitation.externalLinkTitle")}>
            <span className={styles.linkText}>{seg.text}</span>
            <span className={styles.linkTag}>{t("elicitation.externalLink")}</span>
          </span>
        ) : (
          seg.text
        )
      )}
    </>
  );
}

/**
 * An MCP server's form for the owner (`elicitation_requested`). The server's message, field
 * titles and descriptions are untrusted text: shown as plain text nodes, never as markup, and web
 * addresses never as links. A suspected credential field (`blocked`) is shown without an input and
 * is never sent. The answer goes to that server only; the model never sees it. Inputs are
 * uncontrolled and only the schema's `default` is ever prefilled.
 */
export function ElicitationCard({ item, onAnswer, onCancel }: ElicitationCardProps) {
  const { t } = useTranslation();
  const uid = useId();
  const formRef = useRef<HTMLFormElement>(null);
  const [now, setNow] = useState(() => Date.now());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [invalid, setInvalid] = useState<number | null>(null);
  const [gone, setGone] = useState(false);

  const limited = Number.isFinite(item.expiresMs);
  const left = limited ? secondsLeft(item.expiresMs, now) : Infinity;
  const done = item.outcome !== undefined;
  const expired = left === 0;
  const locked = done || item.closed === true || gone || expired;
  const server = escapeInvisible(item.server);
  // A required credential field can never be answered: the form can only be declined or cancelled.
  const unsatisfiable = hasUnsatisfiableField(item.fields);

  // While the form is open: tick the countdown and move focus to the first field (the card
  // appears on its own, so a screen reader must be sent here).
  useEffect(() => {
    if (locked) return undefined;
    formRef.current?.querySelector<HTMLElement>("input, select, button:not([disabled])")?.focus();
    if (!limited) return undefined;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [locked, limited]);

  const label = (f: ElicitationField) => escapeInvisible(f.title ?? f.name);

  // A 400 names the field the server rejected (the form keeps waiting); anything else is a fixed sentence.
  const describe = (err: unknown): string => {
    if (err instanceof NoClientError) return t("elicitation.notConnected");
    if (err instanceof ApiError && err.status === 400 && !err.host) return escapeInvisible(err.message);
    return humanLine(err);
  };

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (err) {
      if (err instanceof ApiError && isElicitationGone(err.code)) setGone(true);
      setError(describe(err));
    } finally {
      setBusy(false);
    }
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const form = formRef.current;
    if (busy || locked || !form || unsatisfiable) return;
    // A blocked field has no input: nothing is read for it, and buildContent skips it anyway.
    const raw: RawValue[] = item.fields.map((f, n) => {
      if (f.blocked === true) return undefined;
      const el = form.elements.namedItem(`f${n}`) as HTMLInputElement | HTMLSelectElement | null;
      if (!el) return undefined;
      return f.type === "boolean" ? (el as HTMLInputElement).checked : el.value;
    });
    const built = buildContent(item.fields, raw);
    if (!built.ok) {
      const f = built.field;
      const n = item.fields.indexOf(f);
      setInvalid(n);
      (form.elements.namedItem(`f${n}`) as HTMLElement | null)?.focus();
      setError(
        t(`elicitation.problem.${built.problem}`, {
          field: label(f),
          min: f.minimum ?? f.min_length,
          max: f.maximum ?? f.max_length
        })
      );
      return;
    }
    setInvalid(null);
    await run(() => onAnswer({ action: "accept", content: built.content }));
  };

  const decline = () => {
    if (busy || locked) return;
    void run(() => onAnswer({ action: "decline", content: {} }));
  };

  const cancel = () => {
    if (busy || locked) return;
    void run(onCancel);
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      cancel();
    }
  };

  const titleId = `${uid}-title`;
  const messageId = `${uid}-message`;
  const errorId = `${uid}-error`;
  const blockedNoteId = `${uid}-blocked`;

  const field = (f: ElicitationField, n: number) => {
    const id = `${uid}-f${n}`;
    const helpId = f.description ? `${id}-help` : undefined;
    const describedBy = [helpId, invalid === n ? errorId : undefined].filter(Boolean).join(" ") || undefined;
    const common = {
      id,
      name: `f${n}`,
      disabled: busy,
      "aria-describedby": describedBy,
      "aria-invalid": invalid === n ? true : undefined,
      "aria-required": f.required || undefined
    };
    const help = helpId ? (
      <p className={formStyles.hint} id={helpId}>
        <ServerText text={escapeInvisible(f.description ?? "")} />
      </p>
    ) : null;
    const marker = f.required ? (
      <>
        {" "}
        <span aria-hidden="true">*</span>
      </>
    ) : null;
    if (f.blocked === true) {
      // No input at all: there is nothing the owner could type, and nothing to send.
      return (
        <div key={n} className={formStyles.field}>
          <div
            className={styles.blocked}
            role="group"
            aria-labelledby={`${id}-label`}
            aria-describedby={helpId}
            data-testid="elicitation-blocked-field"
          >
            <span className={styles.blockedName} id={`${id}-label`}>{label(f)}{marker}</span>
            <span className={styles.blockedTag}>
              <ShieldAlert size={14} aria-hidden="true" />
              {t("elicitation.blocked")}
            </span>
          </div>
          {help}
        </div>
      );
    }
    if (f.type === "boolean") {
      return (
        <div key={n} className={formStyles.field}>
          <label className={styles.confirm} htmlFor={id}>
            <input type="checkbox" defaultChecked={defaultChecked(f)} {...common} />
            <span>{label(f)}{marker}</span>
          </label>
          {help}
        </div>
      );
    }
    return (
      <div key={n} className={formStyles.field}>
        <label className={formStyles.label} htmlFor={id}>{label(f)}{marker}</label>
        {f.options ? (
          <select className={formStyles.select} defaultValue={defaultText(f) ?? ""} {...common}>
            <option value="">{t("elicitation.choose")}</option>
            {f.options.map((o) => (
              <option key={o.value} value={o.value}>{escapeInvisible(o.label)}</option>
            ))}
          </select>
        ) : (
          <input
            className={formStyles.input}
            type={inputType(f)}
            defaultValue={defaultText(f)}
            min={f.minimum}
            max={f.maximum}
            step={f.type === "integer" ? 1 : f.type === "number" ? "any" : undefined}
            minLength={f.min_length}
            maxLength={f.max_length}
            autoComplete="off"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            {...common}
          />
        )}
        {help}
      </div>
    );
  };

  return (
    <section
      className={styles.card}
      data-testid="elicitation-card"
      data-call-id={item.callId}
      aria-labelledby={titleId}
      aria-describedby={messageId}
    >
      {/* Ours, not the server's: a separate element that stays whatever the card's state. */}
      <p className={styles.banner} data-testid="elicitation-banner">
        <Server size={14} aria-hidden="true" />
        <span>{t("elicitation.banner", { server })}</span>
      </p>

      <div className={styles.head}>
        <h3 className={styles.title} id={titleId}>
          <ClipboardList size={16} aria-hidden="true" />
          {t("elicitation.title", { server })}
        </h3>
        {!locked && limited ? (
          <span className={styles.countdown} role="timer" aria-label={t("elicitation.expiresIn")}>
            {formatCountdown(left)}
          </span>
        ) : null}
      </div>

      <div>
        <span className={styles.messageLabel}>{t("elicitation.messageLabel", { server })}</span>
        <p className={styles.message} id={messageId} data-testid="elicitation-message">
          <ServerText text={escapeInvisible(item.message)} />
        </p>
      </div>
      <p className={styles.status}>{t("elicitation.note", { server })}</p>

      {done ? (
        <p className={styles.status} role="status">{t(`elicitation.outcome.${item.outcome}`)}</p>
      ) : gone ? (
        <p className={styles.status} role="status">{error ?? t("elicitation.closed")}</p>
      ) : locked ? (
        <p className={styles.status} role="status">
          {expired && item.closed !== true ? t("elicitation.expired") : t("elicitation.closed")}
        </p>
      ) : (
        <form
          ref={formRef}
          className={styles.form}
          onSubmit={(e) => void submit(e)}
          onKeyDown={onKeyDown}
          autoComplete="off"
          noValidate
        >
          {item.fields.map(field)}
          {unsatisfiable ? (
            <p className={styles.blockedNote} id={blockedNoteId} data-testid="elicitation-blocked-note">
              {t("elicitation.blockedRequired")}
            </p>
          ) : null}
          <div className={styles.actions}>
            <Button
              size="sm"
              variant="primary"
              type="submit"
              disabled={busy || unsatisfiable}
              aria-describedby={unsatisfiable ? blockedNoteId : undefined}
            >
              {t("elicitation.submit")}
            </Button>
            <Button size="sm" variant="secondary" type="button" disabled={busy} onClick={decline}>
              {t("elicitation.decline")}
            </Button>
            <Button size="sm" variant="ghost" type="button" disabled={busy} onClick={cancel}>
              {t("elicitation.cancel")}
            </Button>
          </div>
          {error ? <p className={formStyles.error} id={errorId} role="alert">{error}</p> : null}
        </form>
      )}
    </section>
  );
}
