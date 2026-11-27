import { Clock3, Mail, Plus, Webhook, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { EmailAccount, IncomingWebhook } from "../api/types";
import { accountsForBot } from "../lib/botMail";
import { DEFAULT_SIMPLE, simpleSpec } from "../lib/scheduleBuilder";
import { specLabel } from "../lib/schedules";
import { humanLine } from "../lib/humanError";
import { clockTime, dailyTime, minutes, newTrigger, triggerInput, triggerSpec, type BotTrigger, type TriggerInput, type TriggerPreview, type TriggerSource } from "../lib/triggers";
import { useDaemon } from "../state/DaemonContext";
import { Switch } from "./SettingsUi";
import { useToast } from "./Toast";
import styles from "./BotPanelTriggers.module.css";

const icons = { mail: Mail, webhook: Webhook, time: Clock3 };

export function BotPanelTriggers({ botId }: { botId: string }) {
  const { t, i18n } = useTranslation();
  const { client } = useDaemon();
  const toast = useToast();
  const loadVersion = useRef<object>({});
  const [rows, setRows] = useState<BotTrigger[]>([]);
  const [accounts, setAccounts] = useState<EmailAccount[]>([]);
  const [hooks, setHooks] = useState<IncomingWebhook[]>([]);
  const [draft, setDraft] = useState<TriggerInput | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [adopting, setAdopting] = useState(false);
  const [preview, setPreview] = useState<TriggerPreview | null>(null);
  const [sample, setSample] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [mailError, setMailError] = useState<string | null>(null);
  const [hookError, setHookError] = useState<string | null>(null);
  const tr = (key: string) => t(`botPanel.triggers.${key}`);
  const load = useCallback(async () => {
    if (!client) return;
    const version={}; loadVersion.current=version;
    try {
      const [rules, mail, webhooks] = await Promise.allSettled([client.listTriggers(botId), client.listEmailAccounts(), client.listIncomingWebhooks(botId)]);
      if (version !== loadVersion.current) return;
      if (rules.status === "rejected") throw rules.reason;
      setRows(rules.value);
      setAccounts(mail.status === "fulfilled" ? accountsForBot(mail.value,botId) : []);
      setHooks(webhooks.status === "fulfilled" ? webhooks.value : []);
      setMailError(mail.status === "rejected" ? humanLine(mail.reason) : null);
      setHookError(webhooks.status === "rejected" ? humanLine(webhooks.reason) : null);
      setError(null);
    } catch (e) { if(version===loadVersion.current) setError(humanLine(e)); }
  }, [botId, client]);
  useEffect(() => { setDraft(null); setRows([]); void load(); const timer=window.setInterval(() => { void load(); },30_000); return () => { loadVersion.current={}; window.clearInterval(timer); }; }, [load]);
  const change = (next: TriggerInput) => { setDraft(next); setPreview(null); };
  const save = async (input: TriggerInput, id: string | null) => {
    if (!client || busy) return;
    setBusy(true);
    try {
      if (id) await client.updateTrigger(botId, id, triggerInput(input));
      else await client.createTrigger(botId, triggerInput(input));
      setDraft(null); setPreview(null); await load();
    } catch (e) { toast.show(humanLine(e), "danger"); } finally { setBusy(false); }
  };
  const source = draft?.config.source;
  const quiet = draft?.config.quiet_hours;
  const daily = source ? dailyTime(source) : null;
  const timing = source?.kind === "time" ? specLabel(source.spec, ms => new Date(ms).toLocaleString(i18n.language), i18n.language) : null;
  const setSource = (next: TriggerSource) => { if (draft) change({ ...draft, config: { ...draft.config, source: next } }); };

  return <section className={styles.section} aria-labelledby="triggers-heading" data-testid="bot-triggers">
    <div className={styles.heading}><h3 id="triggers-heading">{tr("title")}</h3>
      <button type="button" className={styles.iconButton} aria-label={tr("add")} title={tr("add")} data-testid="trigger-add" onClick={() => { setEditing(null); setAdopting(false); setDraft(newTrigger()); setPreview(null); setSample(""); }}><Plus aria-hidden="true" /></button></div>
    <p className={styles.note}>{tr("intro")}</p>
    {error && <p role="alert" className={styles.error}>{error}</p>}
    <ul className={styles.list}>
      {rows.map(row => { const Icon = icons[row.config.source.kind]; return <li key={row.id} className={styles.card}>
        <div className={styles.row}><Icon aria-hidden="true" /><button type="button" className={styles.name} onClick={() => { setEditing(row.origin === "webhook" ? null : row.id); setAdopting(row.origin !== "rule"); setDraft({ ...triggerInput(row), enabled: row.origin === "rule" ? row.enabled : false }); setPreview(null); }}>{row.name}</button>
          <span className={styles.chip}>{row.last_error ? tr("paused") : row.enabled ? tr("on") : tr("off")}</span>
          {row.origin === "rule" && <Switch checked={row.enabled} disabled={busy} label={t("botPanel.triggers.toggle", { name: row.name })} onChange={enabled => void save({ ...row, enabled }, row.id)} />}</div>
        {row.origin === "rule" ? <><p className={styles.note}>{t("botPanel.triggers.count", { count: row.count_today ?? 0 })} · {t("botPanel.triggers.queued", { count: row.queued ?? 0 })}</p>
        <p className={styles.note}>{t("botPanel.triggers.limitSummary", { hour: row.config.limits.per_hour, day: row.config.limits.per_day })}</p></> : <p className={styles.note}>{tr("existingHint")}</p>}
        {row.last_fired_at_ms && <p className={styles.note}>{tr("lastFired")} {new Date(row.last_fired_at_ms).toLocaleString(i18n.language)}</p>}
        {row.last_error && <p className={styles.error}>{tr(row.last_error === "source" ? "sourceError" : row.last_error === "sender" ? "senderError" : row.last_error === "queue" ? "queueError" : "runError")}</p>}
      </li>; })}
    </ul>
    {!rows.length && !draft && <p className={styles.empty}>{tr("empty")}</p>}
    {draft && source && <form className={styles.form} data-testid="trigger-builder" onSubmit={e => { e.preventDefault(); void save(draft, editing); }}>
      <div className={styles.row}><strong>{editing && !adopting ? tr("edit") : tr("add")}</strong><button type="button" className={styles.iconButton} aria-label={tr("close")} title={tr("close")} onClick={() => setDraft(null)}><X aria-hidden="true" /></button></div>
      <label className={styles.field}>{tr("name")}<input required maxLength={80} value={draft.name} data-testid="trigger-name" onChange={e => change({ ...draft, name: e.target.value })} /></label>
      <label className={styles.field}>{tr("when")}<select value={source.kind} data-testid="trigger-kind" onChange={e => setSource(e.target.value === "mail" ? { kind: "mail", account: "", folder: "INBOX", own_address: "", poll_seconds: 120 } : e.target.value === "webhook" ? { kind: "webhook", id: "" } : { kind: "time", spec: { cron: `0 9 * * * ${Intl.DateTimeFormat().resolvedOptions().timeZone}` } })}>
        <option value="mail">{tr("mail")}</option><option value="webhook">{tr("webhook")}</option><option value="time">{tr("time")}</option></select></label>
      {source.kind === "mail" && <>
        {mailError && <p role="alert" className={styles.error}>{mailError}</p>}
        <label className={styles.field}>{tr("account")}<select required value={source.account} onChange={e => { const a = accounts.find(a => a.name === e.target.value); setSource({ ...source, account: e.target.value, own_address: a?.address ?? "" }); }}><option value="">{tr("chooseAccount")}</option>{accounts.filter(a => a.provider === "imap_smtp").map(a => <option key={a.name} value={a.name}>{a.address}</option>)}</select></label>
        <p className={styles.note}>{tr("mailHint")}</p>
        <div className={styles.pair}><label className={styles.field}>{tr("senderMatch")}<select value={draft.config.filters.sender_match} onChange={e => change({ ...draft, config: { ...draft.config, filters: { ...draft.config.filters, sender_match: e.target.value as "equals" | "domain" } } })}>{["equals", "domain"].map(k => <option key={k} value={k}>{tr(k)}</option>)}</select></label>
          <label className={styles.field}>{tr("sender")}<input required maxLength={256} value={draft.config.filters.sender} onChange={e => change({ ...draft, config: { ...draft.config, filters: { ...draft.config.filters, sender: e.target.value } } })} /></label></div>
        <label className={styles.field}>{tr("subject")}<input maxLength={256} value={draft.config.filters.subject} onChange={e => change({ ...draft, config: { ...draft.config, filters: { ...draft.config.filters, subject: e.target.value } } })} /></label>
        <div className={styles.pair}><label className={styles.field}>{tr("folder")}<input required value={source.folder} onChange={e => setSource({ ...source, folder: e.target.value })} /></label>
          <label className={styles.field}>{tr("poll")}<input type="number" min={60} max={86400} value={source.poll_seconds} onChange={e => setSource({ ...source, poll_seconds: Number(e.target.value) })} /></label></div>
        <label className={styles.check}><input type="checkbox" checked={draft.config.filters.has_attachment} onChange={e => change({ ...draft, config: { ...draft.config, filters: { ...draft.config.filters, has_attachment: e.target.checked } } })} />{tr("attachment")}</label>
      </>}
      {source.kind === "webhook" && <>{hookError && <p role="alert" className={styles.error}>{hookError}</p>}<label className={styles.field}>{tr("hook")}<select required value={source.id} onChange={e => setSource({ ...source, id: e.target.value })}><option value="">{tr("chooseHook")}</option>{hooks.map(h => <option key={h.id} value={h.id}>{h.name}</option>)}</select></label><p className={styles.note}>{tr("hookHint")}</p></>}
      {source.kind === "time" && <>
        {daily && <div className={styles.pair}><label className={styles.field}>{tr("dailyAt")}<input type="time" required value={daily.time} onChange={e => { const spec=simpleSpec({ ...DEFAULT_SIMPLE, time:e.target.value }, daily.zone); if(spec) setSource({ kind:"time",spec }); }} /></label>
        <label className={styles.field}>{tr("timezone")}<input required value={daily.zone} onChange={e => { const spec=simpleSpec({ ...DEFAULT_SIMPLE,time:daily.time },e.target.value); if(spec) setSource({ kind:"time",spec }); }} /></label></div>}
        {!daily && timing && <p className={styles.note}>{t(timing.i18nKey, timing.values)}</p>}
        <details open={!daily || undefined}><summary>{tr("customSchedule")}</summary><label className={styles.field}>{tr("schedule")}<input value={"cron" in source.spec ? source.spec.cron : JSON.stringify(source.spec)} onChange={e => setSource({ ...source,spec:triggerSpec(e.target.value) })} /><span className={styles.note}>{tr("scheduleHint")}</span></label></details>
      </>}
      <label className={styles.field}>{tr("should")}<textarea required maxLength={4000} value={draft.instruction} data-testid="trigger-instruction" onChange={e => change({ ...draft, instruction: e.target.value })} /></label>
      <div className={styles.examples}>{["exampleSummary", "exampleDraft", "exampleNotify"].map(k => <button type="button" key={k} onClick={() => change({ ...draft, instruction: tr(k) })}>{tr(k)}</button>)}</div>
      <details><summary>{tr("quietHours")}{quiet ? ` · ${clockTime(quiet.start)}–${clockTime(quiet.end)}` : ` · ${tr("off")}`}</summary>
        <label className={styles.check}><input type="checkbox" checked={Boolean(quiet)} onChange={e => change({ ...draft, config: { ...draft.config, quiet_hours: e.target.checked ? { start: 1320, end: 480, timezone: Intl.DateTimeFormat().resolvedOptions().timeZone, drop: false } : null } })} />{tr("useQuiet")}</label>
        {quiet && <><div className={styles.pair}>{(["start", "end"] as const).map(k => <label key={k} className={styles.field}>{tr(k)}<input type="time" value={clockTime(quiet[k])} onChange={e => change({ ...draft, config: { ...draft.config, quiet_hours: { ...quiet, [k]: minutes(e.target.value) } } })} /></label>)}</div>
          <label className={styles.field}>{tr("timezone")}<input required value={quiet.timezone} onChange={e => change({ ...draft, config: { ...draft.config, quiet_hours: { ...quiet, timezone: e.target.value } } })} /></label>
          <label className={styles.field}>{tr("duringQuiet")}<select value={quiet.drop ? "drop" : "digest"} onChange={e => change({ ...draft, config: { ...draft.config, quiet_hours: { ...quiet, drop: e.target.value === "drop" } } })}><option value="digest">{tr("digest")}</option><option value="drop">{tr("drop")}</option></select></label></>}
      </details>
      <details><summary>{tr("limits")} · {t("botPanel.triggers.limitSummary", { hour: draft.config.limits.per_hour, day: draft.config.limits.per_day })}</summary>
        {(["limits", "bot_limits"] as const).map(group => <fieldset key={group}><legend>{tr(group === "limits" ? "thisRule" : "thisBot")}</legend><div className={styles.pair}>{(["per_hour", "per_day"] as const).map(k => <label key={k} className={styles.field}>{tr(k)}<input required type="number" min={1} max={k === "per_hour" ? 1000 : 10000} value={draft.config[group][k]} onChange={e => change({ ...draft, config: { ...draft.config, [group]: { ...draft.config[group], [k]: Number(e.target.value) } } })} /></label>)}</div></fieldset>)}
        <p className={styles.note}>{tr("limitsHint")}</p>
      </details>
      <details><summary>{tr("test")}</summary><label className={styles.field}>{tr("sample")}<textarea value={sample} onChange={e => { setSample(e.target.value); setPreview(null); }} /></label>
        <button type="button" className={styles.secondary} disabled={busy || !draft.instruction.trim()} onClick={() => {
          if (!client) return;
          setBusy(true);
          const mail = source.kind === "mail";
          const sender = draft.config.filters.sender_match === "domain" ? `person@${draft.config.filters.sender.replace(/^@/, "")}` : draft.config.filters.sender;
          const event = mail ? { uid: 1, from: sender, from_addr: sender.toLowerCase(), subject: sample || draft.config.filters.subject, automation_headers: {}, has_attachment: true } : { body: sample, received_at: 0 };
          void client.previewTrigger(botId, { instruction: draft.instruction, config: draft.config, sample: event }).then(setPreview).catch(e => toast.show(humanLine(e), "danger")).finally(() => setBusy(false));
        }}>{tr("preview")}</button>
        {preview && <><p className={styles.note}>{tr(preview.matches ? "sampleMatches" : "sampleMisses")}</p><pre className={styles.preview} data-testid="trigger-preview">{preview.text}</pre></>}
        <p className={styles.note}>{tr("previewHint")}</p>
      </details>
      <p className={styles.note}>{tr("approvalHint")}</p>
      <div className={styles.row}><button type="submit" className={styles.primary} disabled={busy}>{editing && !adopting ? tr("save") : tr("saveOff")}</button>
        {editing && !adopting && <button type="button" className={styles.secondary} disabled={busy} onClick={() => { if (!client) return; setBusy(true); void client.deleteTrigger(botId, editing).then(() => { setDraft(null); return load(); }).catch(e => toast.show(humanLine(e), "danger")).finally(() => setBusy(false)); }}>{tr("remove")}</button>}</div>
    </form>}
  </section>;
}
