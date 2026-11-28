import { useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { PhoneApprovalStatus } from "../api/types";
import form from "../components/Form.module.css";
import { humanLine } from "../lib/humanError";
import { setupProblem } from "../lib/phoneApproval";
import { useDaemon } from "../state/DaemonContext";
import styles from "./NotificationSettings.module.css";

/**
 * Settings > Notifications: approve payments from the phone (ADR
 * docs/design/phone-money-approval-adr.md). The owner sets a PIN (the second factor, stored on
 * the daemon as a hash) and the public address of their tunnel. A Telegram money card then
 * carries a one-time link to a page that asks for this PIN; the chat itself can never approve.
 */
export function PhoneApprovalSettings() {
  const { t, i18n } = useTranslation();
  const { client } = useDaemon();
  const idBase = useId();
  const [status, setStatus] = useState<PhoneApprovalStatus | null>(null);
  const [address, setAddress] = useState("");
  const [pin, setPin] = useState("");
  const [repeat, setRepeat] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    if (!client) return undefined;
    let cancelled = false;
    client
      .phoneApproval()
      .then((s) => {
        if (cancelled) return;
        setStatus(s);
        setAddress(s.address ?? "");
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(humanLine(e));
      });
    return () => {
      cancelled = true;
    };
  }, [client]);

  if (!status) {
    return error ? (
      <p role="alert" className={form.error}>
        {error}
      </p>
    ) : null;
  }

  const problem = setupProblem(address, pin, repeat, status.enabled);
  const run = async (task: () => Promise<PhoneApprovalStatus>) => {
    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      const next = await task();
      setStatus(next);
      setAddress(next.address ?? "");
      setSaved(true);
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setPin("");
      setRepeat("");
      setBusy(false);
    }
  };
  const save = () => {
    if (!client || problem) return;
    void run(() =>
      client.putPhoneApproval({ address: address.trim(), ...(pin ? { pin } : {}) })
    );
  };
  const turnOff = () => {
    if (!client) return;
    void run(() => client.deletePhoneApproval());
  };
  const lockedUntil = status.locked_until_ms
    ? new Intl.DateTimeFormat(i18n.language, { timeStyle: "short" }).format(
        new Date(status.locked_until_ms)
      )
    : null;

  return (
    <div className={styles.group} data-testid="phone-approval">
      <h3 className={styles.subHeading}>{t("phoneApproval.title")}</h3>
      <p className={form.hint}>{t("phoneApproval.description")}</p>
      <p className={form.hint} data-testid="phone-approval-state">
        {status.enabled ? t("phoneApproval.on") : t("phoneApproval.off")}
      </p>
      {lockedUntil ? (
        <p role="status" className={form.error} data-testid="phone-approval-locked">
          {t("phoneApproval.locked", { time: lockedUntil })}
        </p>
      ) : null}
      <label className={form.field} htmlFor={`${idBase}-address`}>
        <span className={form.label}>{t("phoneApproval.address")}</span>
        <input
          id={`${idBase}-address`}
          className={form.input}
          type="url"
          inputMode="url"
          autoComplete="off"
          spellCheck={false}
          placeholder="https://pay.example.org"
          value={address}
          disabled={busy}
          aria-invalid={address !== "" && problem === "phoneApproval.badAddress"}
          onChange={(e) => setAddress(e.target.value)}
          data-testid="phone-approval-address"
        />
        <span className={form.hint}>{t("phoneApproval.addressHint")}</span>
      </label>
      <div className={styles.times}>
        <label className={form.field} htmlFor={`${idBase}-pin`}>
          <span className={form.label}>
            {status.enabled ? t("phoneApproval.newPin") : t("phoneApproval.pin")}
          </span>
          <input
            id={`${idBase}-pin`}
            className={form.input}
            type="password"
            inputMode="numeric"
            autoComplete="new-password"
            maxLength={12}
            value={pin}
            disabled={busy}
            onChange={(e) => setPin(e.target.value.replace(/\D/g, ""))}
            data-testid="phone-approval-pin"
          />
        </label>
        <label className={form.field} htmlFor={`${idBase}-repeat`}>
          <span className={form.label}>{t("phoneApproval.repeatPin")}</span>
          <input
            id={`${idBase}-repeat`}
            className={form.input}
            type="password"
            inputMode="numeric"
            autoComplete="new-password"
            maxLength={12}
            value={repeat}
            disabled={busy}
            onChange={(e) => setRepeat(e.target.value.replace(/\D/g, ""))}
            data-testid="phone-approval-repeat"
          />
        </label>
      </div>
      <p className={form.hint}>{t("phoneApproval.pinHint")}</p>
      {problem && (address !== "" || pin !== "" || repeat !== "") ? (
        <p className={form.hint} data-testid="phone-approval-problem">
          {t(problem)}
        </p>
      ) : null}
      <div className={form.row}>
        <Button
          size="sm"
          disabled={busy || problem !== null}
          onClick={save}
          data-testid="phone-approval-save"
        >
          {status.enabled ? t("phoneApproval.save") : t("phoneApproval.turnOn")}
        </Button>
        {status.enabled ? (
          <Button
            size="sm"
            variant="secondary"
            disabled={busy}
            onClick={turnOff}
            data-testid="phone-approval-off"
          >
            {t("phoneApproval.turnOff")}
          </Button>
        ) : null}
      </div>
      {saved && !error ? (
        <p role="status" className={form.success}>
          {t("phoneApproval.saved")}
        </p>
      ) : null}
      {error ? (
        <p role="alert" className={form.error}>
          {error}
        </p>
      ) : null}
    </div>
  );
}
