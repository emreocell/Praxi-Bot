import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import { KeyRound } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { ApiError } from "../api/client";
import type { SecretRequestItem } from "../state/timeline";
import { shieldFromCapture } from "../lib/captureShield";
import { escapeInvisible } from "../lib/approvalPlain";
import { humanLine } from "../lib/humanError";
import {
  buildSubmitBody,
  formatCountdown,
  isGone,
  parseAllowList,
  secondsLeft,
  type SubmitBody
} from "../lib/secretRequestForm";
import formStyles from "./Form.module.css";
import styles from "./SecretRequestCard.module.css";

export interface SecretRequestCardProps {
  item: SecretRequestItem;
  /** Which bot is asking (the bot that owns the run). */
  botName: string;
  onSubmit: (body: SubmitBody) => Promise<void>;
  onCancel: () => Promise<void>;
}

/**
 * The owner's form for a bot's `secret_request`. The bot supplies only a name, a one-line purpose
 * and which fields it wants; everything else on this card is the app's own text. The typed value
 * lives only in the (uncontrolled) inputs: it is read once on submit, the inputs are emptied
 * straight away, and it is never put in React state, a log, an error or a URL.
 */
export function SecretRequestCard({ item, botName, onSubmit, onCancel }: SecretRequestCardProps) {
  const { t } = useTranslation();
  const uid = useId();
  const valueRef = useRef<HTMLInputElement | null>(null);
  const userRef = useRef<HTMLInputElement | null>(null);
  const passRef = useRef<HTMLInputElement | null>(null);
  const originsRef = useRef<HTMLTextAreaElement>(null);
  const programsRef = useRef<HTMLTextAreaElement>(null);
  const firstRef = useRef<HTMLInputElement | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [busy, setBusy] = useState(false);
  const [confirmed, setConfirmed] = useState(!item.untrustedContent);
  const [error, setError] = useState<string | null>(null);
  const [gone, setGone] = useState(false);

  const left = secondsLeft(item.expiresMs, now);
  const done = item.outcome !== undefined;
  const expired = left === 0;
  const locked = done || item.closed === true || gone || expired;
  const login = item.fields === "login";

  // While the form is open: tick the countdown, keep the window out of screen capture, and move
  // focus to the first field (the card appears on its own, so a screen reader must be sent here).
  useEffect(() => {
    if (locked) return undefined;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    const release = shieldFromCapture();
    firstRef.current?.focus();
    return () => {
      window.clearInterval(timer);
      release();
    };
  }, [locked]);

  // Whatever happens to the card, nothing typed outlives it.
  const wipe = () => {
    for (const r of [valueRef, userRef, passRef]) {
      if (r.current) r.current.value = "";
    }
  };
  useEffect(() => wipe, []);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy || locked || !confirmed) return;
    const body = buildSubmitBody(
      item.fields,
      {
        value: valueRef.current?.value,
        username: userRef.current?.value,
        password: passRef.current?.value
      },
      parseAllowList(originsRef.current?.value ?? ""),
      parseAllowList(programsRef.current?.value ?? "")
    );
    if (!body) {
      setError(t("secretRequest.fillAll"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await onSubmit(body);
      wipe();
    } catch (err) {
      // The form stays (a 400 or 409 can be corrected); the message never carries the value.
      if (err instanceof ApiError && isGone(err.code)) {
        wipe();
        setGone(true);
      }
      setError(humanLine(err));
    } finally {
      body.value = undefined;
      body.password = undefined;
      setBusy(false);
    }
  };

  const cancel = async () => {
    if (busy || locked) return;
    wipe();
    setBusy(true);
    setError(null);
    try {
      await onCancel();
    } catch (err) {
      if (err instanceof ApiError && isGone(err.code)) setGone(true);
      setError(humanLine(err));
    } finally {
      setBusy(false);
    }
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      void cancel();
    }
  };

  const titleId = `${uid}-title`;
  const purposeId = `${uid}-purpose`;
  const warnId = `${uid}-warn`;
  const type = login ? t("secretRequest.typeLogin") : t("secretRequest.typeSecret");
  const inputProps = {
    autoComplete: "off",
    autoCapitalize: "off",
    autoCorrect: "off",
    spellCheck: false,
    disabled: busy
  } as const;

  return (
    <section
      className={styles.card}
      data-testid="secret-request-card"
      data-call-id={item.callId}
      aria-labelledby={titleId}
      aria-describedby={`${purposeId}${item.untrustedContent ? ` ${warnId}` : ""}`}
    >
      <div className={styles.head}>
        <h3 className={styles.title} id={titleId}>
          <KeyRound size={16} aria-hidden="true" />
          {t("secretRequest.title", { bot: escapeInvisible(botName) })}
        </h3>
        {!locked ? (
          <span className={styles.countdown} role="timer" aria-label={t("secretRequest.expiresIn")}>
            {formatCountdown(left)}
          </span>
        ) : null}
      </div>

      <div>
        <span className={styles.purposeLabel}>{t("secretRequest.purposeLabel")}</span>
        <p className={styles.purpose} id={purposeId} data-testid="secret-request-purpose">
          {escapeInvisible(item.purpose)}
        </p>
      </div>

      <p className={styles.dest}>
        {t("secretRequest.destination", { type })}{" "}
        <span className={styles.mono}>{escapeInvisible(item.name)}</span>
      </p>
      <p className={styles.status}>{t("secretRequest.neverSeen")}</p>

      {item.untrustedContent ? (
        <p className={styles.warn} id={warnId} role="alert">
          {t("secretRequest.untrusted")}
        </p>
      ) : null}
      {item.replacesExisting ? (
        <p className={styles.note} data-testid="secret-request-replaces">
          {t("secretRequest.replaces", { name: escapeInvisible(item.name) })}
        </p>
      ) : null}

      {done ? (
        <p className={styles.status} role="status">
          {item.outcome === "stored" ? t("secretRequest.stored") : t("secretRequest.cancelled")}
        </p>
      ) : gone ? (
        <p className={styles.status} role="status">{error ?? t("secretRequest.closed")}</p>
      ) : locked ? (
        <p className={styles.status} role="status">
          {expired && item.closed !== true ? t("secretRequest.expired") : t("secretRequest.closed")}
        </p>
      ) : (
        <form className={styles.form} onSubmit={(e) => void submit(e)} onKeyDown={onKeyDown} autoComplete="off">
          {login ? (
            <>
              <div className={formStyles.field}>
                <label className={formStyles.label} htmlFor={`${uid}-user`}>{t("secretRequest.username")}</label>
                <input
                  id={`${uid}-user`}
                  ref={(el) => {
                    userRef.current = el;
                    firstRef.current = el;
                  }}
                  className={formStyles.input}
                  type="text"
                  {...inputProps}
                />
              </div>
              <div className={formStyles.field}>
                <label className={formStyles.label} htmlFor={`${uid}-pass`}>{t("secretRequest.password")}</label>
                <input
                  id={`${uid}-pass`}
                  ref={passRef}
                  className={formStyles.input}
                  type="password"
                  {...inputProps}
                />
              </div>
            </>
          ) : (
            <div className={formStyles.field}>
              <label className={formStyles.label} htmlFor={`${uid}-value`}>{t("secretRequest.value")}</label>
              <input
                id={`${uid}-value`}
                ref={(el) => {
                  valueRef.current = el;
                  firstRef.current = el;
                }}
                className={formStyles.input}
                type="password"
                {...inputProps}
              />
            </div>
          )}
          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor={`${uid}-origins`}>{t("secretRequest.origins")}</label>
            <textarea
              id={`${uid}-origins`}
              ref={originsRef}
              className={formStyles.textarea}
              rows={2}
              placeholder="https://example.com"
              spellCheck={false}
              disabled={busy}
            />
          </div>
          <div className={formStyles.field}>
            <label className={formStyles.label} htmlFor={`${uid}-programs`}>{t("secretRequest.programs")}</label>
            <textarea
              id={`${uid}-programs`}
              ref={programsRef}
              className={formStyles.textarea}
              rows={1}
              spellCheck={false}
              disabled={busy}
            />
            <p className={formStyles.hint}>{t("secretRequest.allowHint")}</p>
          </div>
          {item.untrustedContent ? (
            <label className={styles.confirm}>
              <input
                type="checkbox"
                checked={confirmed}
                onChange={(e) => setConfirmed(e.target.checked)}
                disabled={busy}
              />
              <span>{t("secretRequest.confirmUntrusted")}</span>
            </label>
          ) : null}
          <div className={styles.actions}>
            <Button size="sm" variant="primary" type="submit" disabled={busy || !confirmed}>
              {t("secretRequest.submit")}
            </Button>
            <Button size="sm" variant="secondary" type="button" disabled={busy} onClick={() => void cancel()}>
              {t("secretRequest.cancel")}
            </Button>
          </div>
          {error ? <p className={formStyles.error} role="alert">{error}</p> : null}
        </form>
      )}
    </section>
  );
}
