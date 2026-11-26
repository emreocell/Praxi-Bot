import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { SiteRule, SiteRuleSuggestion } from "../../api/types";
import form from "../../components/Form.module.css";
import { humanLine } from "../../lib/humanError";
import { formatDateTime } from "../../lib/messageTime";
import { siteRuleExpiryHours, siteRuleTarget, type SiteRuleExpiry } from "../../lib/siteRules";
import { useDaemon } from "../../state/DaemonContext";
import styles from "./SiteRulesCard.module.css";

const ACTIONS = ["ask", "block", "allow"] as const;
const CAPABILITIES = ["read", "interact", "submit", "any"] as const;
const EXPIRIES: SiteRuleExpiry[] = ["never", "day", "week", "month"];

/** Per-site and per-app rules of one bot: always ask, block, or allow without asking. */
export function SiteRulesCard() {
  const { t, i18n } = useTranslation();
  const { client, bots } = useDaemon();
  const [bot, setBot] = useState("");
  const activeBot = bot || bots[0]?.id || "";
  const [rules, setRules] = useState<SiteRule[]>([]);
  const [recent, setRecent] = useState<SiteRuleSuggestion[]>([]);
  const [site, setSite] = useState("");
  const [action, setAction] = useState<(typeof ACTIONS)[number]>("ask");
  const [capability, setCapability] = useState<(typeof CAPABILITIES)[number]>("submit");
  const [subdomains, setSubdomains] = useState(true);
  const [expiry, setExpiry] = useState<SiteRuleExpiry>("never");
  const [confirming, setConfirming] = useState<SiteRuleSuggestion | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);

  const load = useCallback(async () => {
    if (!client || !activeBot) return;
    try {
      const [r, s] = await Promise.all([client.listSiteRules(activeBot), client.recentSiteApprovals(activeBot)]);
      setRules(r);
      setRecent(s);
    } catch (e) {
      setMsg({ ok: false, text: humanLine(e) });
    }
  }, [client, activeBot]);

  useEffect(() => {
    void load();
  }, [load]);

  const run = async (work: () => Promise<void>, done: string) => {
    if (busy) return;
    setBusy(true);
    setMsg(null);
    try {
      await work();
      setMsg({ ok: true, text: done });
      await load();
    } catch (e) {
      setMsg({ ok: false, text: humanLine(e) });
    } finally {
      setBusy(false);
    }
  };

  // An allow rule covers one host and one kind of action; a broad rule only ever tightens.
  const allowing = action === "allow";
  const effectiveCapability = allowing && capability === "any" ? "interact" : capability;
  const target = (kind: string, key: string) => siteRuleTarget(kind, key);
  const what = (c: string) => t(`settingsModal.siteRules.capabilities.${c}`);

  const add = () =>
    run(async () => {
      await client?.addSiteRule(activeBot, {
        kind: "site",
        key: site.trim(),
        include_subdomains: !allowing && subdomains,
        capability: effectiveCapability,
        action,
        expires_in_hours: siteRuleExpiryHours(expiry)
      });
      setSite("");
    }, t("settingsModal.siteRules.added"));

  const allowSuggestion = (s: SiteRuleSuggestion) =>
    run(async () => {
      await client?.addSiteRule(activeBot, {
        kind: s.kind,
        key: s.key,
        include_subdomains: false,
        capability: s.capability,
        action: "allow",
        expires_in_hours: siteRuleExpiryHours(expiry)
      });
      setConfirming(null);
    }, t("settingsModal.siteRules.added"));

  return (
    <div className={styles.root} data-testid="site-rules">
      <p className={form.hint}>{t("settingsModal.siteRules.scope")}</p>
      <label className={form.field}>
        {t("settingsModal.siteRules.bot")}
        <select className={form.select} value={activeBot} disabled={busy} onChange={(e) => setBot(e.target.value)}>
          {bots.map((b) => (
            <option key={b.id} value={b.id}>
              {b.name}
            </option>
          ))}
        </select>
      </label>
      {!client ? <p className={form.hint}>{t("settingsModal.safety.offline")}</p> : null}
      {msg ? (
        <p className={msg.ok ? form.success : form.error} role={msg.ok ? "status" : "alert"}>
          {msg.text}
        </p>
      ) : null}

      {rules.length === 0 ? (
        <p className={form.hint}>{t("settingsModal.siteRules.empty")}</p>
      ) : (
        <ul className={styles.list} data-testid="site-rules-list">
          {rules.map((r) => (
            <li key={r.id} className={styles.rule}>
              <span className={styles.ruleText}>
                <strong>{t(`settingsModal.siteRules.actions.${r.action}`)}</strong>
                {" · "}
                <code>{target(r.kind, r.key)}</code>
                {r.include_subdomains ? ` ${t("settingsModal.siteRules.withSubdomains")}` : ""}
                {" · "}
                {what(r.capability)}
                {r.expires_at_ms
                  ? ` · ${
                      r.expired
                        ? t("settingsModal.siteRules.expired")
                        : t("settingsModal.siteRules.until", { when: formatDateTime(r.expires_at_ms, i18n.language) })
                    }`
                  : ""}
              </span>
              <Button
                variant="ghost"
                disabled={busy}
                onClick={() => void run(() => client?.removeSiteRule(activeBot, r.id) ?? Promise.resolve(), t("settingsModal.siteRules.removed"))}
              >
                {t("settingsModal.siteRules.remove")}
              </Button>
            </li>
          ))}
        </ul>
      )}

      <h4 className={styles.sub}>{t("settingsModal.siteRules.addTitle")}</h4>
      <div className={styles.form}>
        <label className={form.field}>
          {t("settingsModal.siteRules.site")}
          <input
            className={form.input}
            value={site}
            placeholder="example.com"
            maxLength={253}
            onChange={(e) => setSite(e.target.value)}
          />
        </label>
        <p className={form.hint}>{t("settingsModal.siteRules.siteHint")}</p>
        <label className={form.field}>
          {t("settingsModal.siteRules.action")}
          <select className={form.select} value={action} onChange={(e) => setAction(e.target.value as typeof action)}>
            {ACTIONS.map((a) => (
              <option key={a} value={a}>
                {t(`settingsModal.siteRules.actions.${a}`)}
              </option>
            ))}
          </select>
        </label>
        <label className={form.field}>
          {t("settingsModal.siteRules.capability")}
          <select className={form.select} value={effectiveCapability} onChange={(e) => setCapability(e.target.value as typeof capability)}>
            {CAPABILITIES.filter((c) => !(allowing && c === "any")).map((c) => (
              <option key={c} value={c}>
                {what(c)}
              </option>
            ))}
          </select>
        </label>
        {!allowing ? (
          <label className={styles.check}>
            <input type="checkbox" checked={subdomains} onChange={(e) => setSubdomains(e.target.checked)} />
            {t("settingsModal.siteRules.subdomains")}
          </label>
        ) : (
          <p className={form.hint}>{t("settingsModal.siteRules.allowHint")}</p>
        )}
        <label className={form.field}>
          {t("settingsModal.siteRules.expiry")}
          <select className={form.select} value={expiry} onChange={(e) => setExpiry(e.target.value as SiteRuleExpiry)}>
            {EXPIRIES.map((x) => (
              <option key={x} value={x}>
                {t(`settingsModal.siteRules.expiries.${x}`)}
              </option>
            ))}
          </select>
        </label>
        <div>
          <Button disabled={busy || !client || !activeBot || site.trim() === ""} onClick={() => void add()}>
            {t("settingsModal.siteRules.addButton")}
          </Button>
        </div>
      </div>

      <h4 className={styles.sub}>{t("settingsModal.siteRules.recentTitle")}</h4>
      <p className={form.hint}>{t("settingsModal.siteRules.recentHint")}</p>
      {recent.length === 0 ? (
        <p className={form.hint}>{t("settingsModal.siteRules.recentNone")}</p>
      ) : (
        <ul className={styles.list} data-testid="site-rules-recent">
          {recent.map((s) => (
            <li key={`${s.kind}|${s.key}|${s.capability}`} className={styles.rule}>
              <span className={styles.ruleText}>
                <code>{target(s.kind, s.key)}</code>
                {" · "}
                {what(s.capability)}
                {" · "}
                {formatDateTime(s.approved_at_ms, i18n.language)}
              </span>
              {confirming !== null && confirming.key === s.key && confirming.capability === s.capability && confirming.kind === s.kind ? (
                <span className={styles.confirm} role="group" aria-label={t("settingsModal.siteRules.confirm", { target: siteRuleTarget(s.kind, s.key, true), what: what(s.capability) })}>
                  <span>{t("settingsModal.siteRules.confirm", { target: siteRuleTarget(s.kind, s.key, true), what: what(s.capability) })}</span>
                  <Button disabled={busy} onClick={() => void allowSuggestion(s)}>
                    {t("settingsModal.siteRules.confirmYes")}
                  </Button>
                  <Button variant="ghost" onClick={() => setConfirming(null)}>
                    {t("settingsModal.siteRules.confirmNo")}
                  </Button>
                </span>
              ) : (
                <Button variant="ghost" disabled={busy} onClick={() => setConfirming(s)}>
                  {s.kind === "app" ? t("settingsModal.siteRules.allowApp") : t("settingsModal.siteRules.allowSite")}
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
      <p className={form.hint}>{t("settingsModal.siteRules.limits")}</p>
    </div>
  );
}
