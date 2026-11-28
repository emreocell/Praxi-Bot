import { X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { CalendarAccount, EmailAccount, IncomingWebhook } from "../api/types";
import { accountsForBot } from "../lib/botMail";
import { DEFAULT_SIMPLE, simpleSpec } from "../lib/scheduleBuilder";
import { specLabel } from "../lib/schedules";
import { humanLine } from "../lib/humanError";
import { EVENT_KINDS, WEB_MIN_POLL_SECONDS, clockTime, dailyTime, minutes, newSource, sampleEvent, triggerInput, triggerSpec, type BotTrigger, type TriggerInput, type TriggerKind, type TriggerPreview, type TriggerSource } from "../lib/triggers";
import { useDaemon } from "../state/DaemonContext";
import { useToast } from "./Toast";
import styles from "./BotPanelTriggers.module.css";

/** What the builder opens on: a new rule, or an existing one (`adopting`: shown by the daemon, saved as a new rule). */
export interface TriggerEdit {
  draft: TriggerInput;
  editing: string | null;
  adopting: boolean;
  /** Why the rule stopped, shown on top until it is saved again. */
  lastError?: BotTrigger["last_error"];
}

/**
 * The rule builder of the Automation tab: when (mail / webhook / feed / page / calendar / Home Assistant / time), what the bot should do,
 * then the optional parts folded (quiet hours, limits, a dry run). The list lives in
 * `BotPanelAutomation`; this is only the form.
 */
export function TriggerBuilder({ botId, start, onClose, onSaved }: { botId: string; start: TriggerEdit; onClose: () => void; onSaved: () => Promise<void> }) {
  const { t, i18n } = useTranslation();
  const { client } = useDaemon();
  const toast = useToast();
  const [draft, setDraft] = useState<TriggerInput>(start.draft);
  const { editing, adopting } = start;
  const [accounts, setAccounts] = useState<EmailAccount[]>([]);
  const [hooks, setHooks] = useState<IncomingWebhook[]>([]);
  const [calendars, setCalendars] = useState<CalendarAccount[]>([]);
  const [preview, setPreview] = useState<TriggerPreview | null>(null);
  const [sample, setSample] = useState("");
  const [busy, setBusy] = useState(false);
  const [mailError, setMailError] = useState<string | null>(null);
  const [hookError, setHookError] = useState<string | null>(null);
  const titleRef = useRef<HTMLHeadingElement | null>(null);
  const tr = (key: string) => t(`botPanel.triggers.${key}`);
  useEffect(() => {
    if (!client) return;
    let live = true;
    void Promise.allSettled([client.listEmailAccounts(), client.listIncomingWebhooks(botId), client.listCalendarAccounts()]).then(([mail, webhooks, calendar]) => {
      if (!live) return;
      setCalendars(calendar.status === "fulfilled" ? calendar.value : []);
      setAccounts(mail.status === "fulfilled" ? accountsForBot(mail.value, botId) : []);
      setHooks(webhooks.status === "fulfilled" ? webhooks.value : []);
      setMailError(mail.status === "rejected" ? humanLine(mail.reason) : null);
      setHookError(webhooks.status === "rejected" ? humanLine(webhooks.reason) : null);
    });
    return () => { live = false; };
  }, [botId, client]);
  // The form opens under the list: bring it into view and put the keyboard on its title.
  useEffect(() => {
    titleRef.current?.scrollIntoView?.({ block: "nearest" });
    titleRef.current?.focus({ preventScroll: true });
  }, []);
  const change = (next: TriggerInput) => { setDraft(next); setPreview(null); };
  const save = async () => {
    if (!client || busy) return;
    setBusy(true);
    try {
      if (editing && !adopting) await client.updateTrigger(botId, editing, triggerInput(draft));
      else await client.createTrigger(botId, triggerInput(draft));
      await onSaved();
    } catch (e) { toast.show(humanLine(e), "danger"); } finally { setBusy(false); }
  };
  const source = draft.config.source;
  const quiet = draft.config.quiet_hours;
  const daily = dailyTime(source);
  const timing = source.kind === "time" ? specLabel(source.spec, ms => new Date(ms).toLocaleString(i18n.language), i18n.language) : null;
  const setSource = (next: TriggerSource) => change({ ...draft, config: { ...draft.config, source: next } });
  const heading = editing && !adopting ? tr("edit") : t(`automation.builder.title.${source.kind}`);

  return <form className={styles.form} data-testid="trigger-builder" aria-label={heading} onSubmit={e => { e.preventDefault(); void save(); }}>
      <div className={styles.row}><h4 className={styles.formTitle} ref={titleRef} tabIndex={-1}>{heading}</h4><button type="button" className={styles.iconButton} aria-label={tr("close")} title={tr("close")} onClick={onClose}><X aria-hidden="true" /></button></div>
      {start.lastError && <p role="alert" className={styles.error}>{tr(start.lastError === "source" ? "sourceError" : start.lastError === "sender" ? "senderError" : start.lastError === "queue" ? "queueError" : "runError")}</p>}
      {adopting && <p className={styles.note}>{tr("existingHint")}</p>}
      <label className={styles.field}>{tr("when")}<select value={source.kind} data-testid="trigger-kind" onChange={e => setSource(newSource(e.target.value as TriggerKind))}>
        {[...EVENT_KINDS, "time" as const].map(k => <option key={k} value={k}>{tr(k === "mail" || k === "webhook" || k === "time" ? k : `when_${k}`)}</option>)}</select></label>
      {source.kind === "mail" && <>
        {mailError && <p role="alert" className={styles.error}>{mailError}</p>}
        <label className={styles.field}>{tr("account")}<select required value={source.account} onChange={e => { const a = accounts.find(a => a.name === e.target.value); setSource({ ...source, account: e.target.value, own_address: a?.address ?? "" }); }}><option value="">{tr("chooseAccount")}</option>{accounts.filter(a => a.provider === "imap_smtp").map(a => <option key={a.name} value={a.name}>{a.address}</option>)}</select></label>
        <p className={styles.note}>{tr("mailHint")}</p>
        <div className={styles.pair}><label className={styles.field}>{tr("senderMatch")}<select value={draft.config.filters.sender_match} onChange={e => change({ ...draft, config: { ...draft.config, filters: { ...draft.config.filters, sender_match: e.target.value as "equals" | "domain" } } })}>{["equals", "domain"].map(k => <option key={k} value={k}>{tr(k)}</option>)}</select></label>
          <label className={styles.field}>{tr("sender")}<input required maxLength={256} value={draft.config.filters.sender} placeholder={draft.config.filters.sender_match === "domain" ? "example.com" : "ann@example.com"} onChange={e => change({ ...draft, config: { ...draft.config, filters: { ...draft.config.filters, sender: e.target.value } } })} /></label></div>
        <label className={styles.field}>{tr("subject")}<input maxLength={256} value={draft.config.filters.subject} onChange={e => change({ ...draft, config: { ...draft.config, filters: { ...draft.config.filters, subject: e.target.value } } })} /></label>
        <label className={styles.check}><input type="checkbox" checked={draft.config.filters.has_attachment} onChange={e => change({ ...draft, config: { ...draft.config, filters: { ...draft.config.filters, has_attachment: e.target.checked } } })} />{tr("attachment")}</label>
        <details><summary>{t("automation.builder.mailbox")}</summary>
          <div className={styles.pair}><label className={styles.field}>{tr("folder")}<input required value={source.folder} onChange={e => setSource({ ...source, folder: e.target.value })} /></label>
            <label className={styles.field}>{tr("poll")}<input type="number" min={60} max={86400} value={source.poll_seconds} onChange={e => setSource({ ...source, poll_seconds: Number(e.target.value) })} /></label></div>
        </details>
      </>}
      {source.kind === "webhook" && <>{hookError && <p role="alert" className={styles.error}>{hookError}</p>}<label className={styles.field}>{tr("hook")}<select required value={source.id} onChange={e => setSource({ ...source, id: e.target.value })}><option value="">{tr("chooseHook")}</option>{hooks.map(h => <option key={h.id} value={h.id}>{h.name}</option>)}</select></label><p className={styles.note}>{hooks.length ? tr("hookHint") : t("automation.builder.noHooks")}</p></>}
      {(source.kind === "feed" || source.kind === "page") && <>
        <label className={styles.field}>{tr("url")}<input required type="url" maxLength={2048} value={source.url} placeholder="https://example.com/feed.xml" data-testid="trigger-url" onChange={e => setSource({ ...source, url: e.target.value })} /></label>
        {source.kind === "page" && <label className={styles.field}>{tr("selector")}<input required maxLength={256} value={source.selector} placeholder="#price" data-testid="trigger-selector" onChange={e => setSource({ ...source, selector: e.target.value })} /><span className={styles.note}>{tr("selectorHint")}</span></label>}
        <p className={styles.note}>{tr(source.kind === "feed" ? "feedHint" : "pageHint")}</p>
        <label className={styles.field}>{tr("pollMinutes")}<input type="number" min={WEB_MIN_POLL_SECONDS / 60} max={1440} value={Math.round(source.poll_seconds / 60)} onChange={e => setSource({ ...source, poll_seconds: Number(e.target.value) * 60 })} /></label>
      </>}
      {source.kind === "calendar" && <>
        <label className={styles.field}>{tr("calendar")}<select value={source.account ?? ""} onChange={e => setSource({ ...source, account: e.target.value || null })}><option value="">{tr("anyCalendar")}</option>{calendars.map(c => <option key={c.name} value={c.name}>{c.name}</option>)}</select></label>
        <label className={styles.field}>{tr("minutesBefore")}<input required type="number" min={1} max={1440} value={source.minutes_before} data-testid="trigger-minutes" onChange={e => setSource({ ...source, minutes_before: Number(e.target.value) })} /></label>
        <p className={styles.note}>{tr("calendarHint")}</p>
      </>}
      {source.kind === "home_assistant" && <>
        <label className={styles.field}>{tr("entity")}<input required maxLength={128} value={source.entity_id} placeholder="binary_sensor.front_door" data-testid="trigger-entity" onChange={e => setSource({ ...source, entity_id: e.target.value })} /></label>
        <label className={styles.field}>{tr("toState")}<input maxLength={255} value={source.to_state ?? ""} placeholder="on" onChange={e => setSource({ ...source, to_state: e.target.value || null })} /><span className={styles.note}>{tr("toStateHint")}</span></label>
        <p className={styles.note}>{tr("haHint")}</p>
      </>}
      {source.kind === "time" && <>
        {daily && <div className={styles.pair}><label className={styles.field}>{tr("dailyAt")}<input type="time" required value={daily.time} onChange={e => { const spec=simpleSpec({ ...DEFAULT_SIMPLE, time:e.target.value }, daily.zone); if(spec) setSource({ kind:"time",spec }); }} /></label>
        <label className={styles.field}>{tr("timezone")}<input required value={daily.zone} onChange={e => { const spec=simpleSpec({ ...DEFAULT_SIMPLE,time:daily.time },e.target.value); if(spec) setSource({ kind:"time",spec }); }} /></label></div>}
        {!daily && timing && <p className={styles.note}>{t(timing.i18nKey, timing.values)}</p>}
        <details open={!daily || undefined}><summary>{tr("customSchedule")}</summary><label className={styles.field}>{tr("schedule")}<input value={"cron" in source.spec ? source.spec.cron : JSON.stringify(source.spec)} onChange={e => setSource({ ...source,spec:triggerSpec(e.target.value) })} /><span className={styles.note}>{tr("scheduleHint")}</span></label></details>
      </>}
      <label className={styles.field}>{tr("should")}<textarea required maxLength={4000} value={draft.instruction} placeholder={t("automation.builder.shouldPlaceholder")} data-testid="trigger-instruction" onChange={e => change({ ...draft, instruction: e.target.value })} /></label>
      <div className={styles.examples}>{["exampleSummary", "exampleDraft", "exampleNotify"].map(k => <button type="button" key={k} onClick={() => change({ ...draft, instruction: tr(k) })}>{tr(k)}</button>)}</div>
      <label className={styles.field}>{tr("name")}<input required maxLength={80} value={draft.name} placeholder={t("automation.builder.namePlaceholder")} data-testid="trigger-name" onChange={e => change({ ...draft, name: e.target.value })} /></label>
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
          const event = mail ? { uid: 1, from: sender, from_addr: sender.toLowerCase(), subject: sample || draft.config.filters.subject, automation_headers: {}, has_attachment: true } : sampleEvent(source, sample);
          void client.previewTrigger(botId, { instruction: draft.instruction, config: draft.config, sample: event }).then(setPreview).catch(e => toast.show(humanLine(e), "danger")).finally(() => setBusy(false));
        }}>{tr("preview")}</button>
        {preview && <><p className={styles.note}>{tr(preview.matches ? "sampleMatches" : "sampleMisses")}</p><pre className={styles.preview} data-testid="trigger-preview">{preview.text}</pre></>}
        <p className={styles.note}>{tr("previewHint")}</p>
      </details>
      <p className={styles.note}>{tr("approvalHint")}</p>
      <div className={styles.row}><button type="submit" className={styles.primary} disabled={busy} data-testid="trigger-save">{editing && !adopting ? tr("save") : tr("saveOff")}</button>
        {editing && !adopting && <button type="button" className={styles.secondary} disabled={busy} onClick={() => { if (!client) return; setBusy(true); void client.deleteTrigger(botId, editing).then(onSaved).catch(e => toast.show(humanLine(e), "danger")).finally(() => setBusy(false)); }}>{tr("remove")}</button>}</div>
    </form>;
}
