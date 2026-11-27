import { CheckCircle2, CircleSlash, FolderInput, LoaderCircle, XCircle } from "lucide-react";
import { type MutableRefObject, useEffect, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ImportApp, ImportItem, ImportScanInput } from "../api/types";
import { Callout, DialogBody, DialogFooter, DialogHeader, dialogStyles, Field } from "../components/DialogParts";
import formStyles from "../components/Form.module.css";
import { Modal } from "../components/Modal";
import { botView } from "../lib/botProfile";
import { humanLine } from "../lib/humanError";
import {
  applyInstructionResults,
  buildRunInput,
  buildScanInput,
  groupItems,
  IMPORT_NAMED_APPS,
  type ImportOutcome,
  importedServers,
  initialSecretTicks,
  initialTicks,
  instructionsWithoutBot,
  knownReason,
  PERSONA_MAX_CHARS,
  selectedIds,
  tally,
  toggled
} from "../lib/importAssistants";
import { useDaemon } from "../state/DaemonContext";
import styles from "./ImportAssistants.module.css";

type Phase = "intro" | "review" | "done";

/**
 * Import from other assistants: one screen that looks in the known places of Claude, Cursor,
 * VS Code, Codex, OpenClaw and Hermes (on the computer the service runs on), lists what it found
 * and imports what the owner ticks. Nothing is read until the owner presses "Look for setups".
 */
export function ImportAssistantsModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const { t } = useTranslation();
  const closeGuard = useRef<() => void>(onClose);
  return (
    <Modal open={open} onClose={() => closeGuard.current()} label={t("importAssistants.title")} size="md">
      {open ? <ImportDialog closeGuard={closeGuard} onClose={onClose} /> : null}
    </Modal>
  );
}

function ImportDialog({
  closeGuard,
  onClose
}: {
  closeGuard: MutableRefObject<() => void>;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const uid = useId();
  const { client, bots, refreshBots } = useDaemon();
  const [phase, setPhase] = useState<Phase>("intro");
  const [busy, setBusy] = useState<"scan" | "run" | null>(null);
  const [folder, setFolder] = useState("");
  const [scanned, setScanned] = useState<{ input: ImportScanInput; items: ImportItem[] } | null>(null);
  const [ticks, setTicks] = useState<Set<string>>(new Set());
  const [secretTicks, setSecretTicks] = useState<Set<string>>(initialSecretTicks);
  const [botFor, setBotFor] = useState<Record<string, string>>({});
  const [outcomes, setOutcomes] = useState<ImportOutcome[]>([]);
  const [error, setError] = useState<string | null>(null);
  const heading = useRef<HTMLHeadingElement | null>(null);
  const first = useRef(true);

  closeGuard.current = () => {
    if (!busy) onClose();
  };

  // A new screen starts at its heading, so keyboard and screen-reader users follow the flow.
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    heading.current?.focus();
  }, [phase]);

  const items = useMemo(() => scanned?.items ?? [], [scanned]);
  const groups = useMemo(() => groupItems(items), [items]);
  const botIds = useMemo(() => bots.map((b) => b.id), [bots]);
  // With exactly one bot there is nothing to choose; with several, the owner picks.
  const resolvedBot = useMemo(() => {
    const out: Record<string, string> = {};
    for (const item of items) {
      if (item.kind === "instructions") out[item.id] = botFor[item.id] ?? (bots.length === 1 ? bots[0].id : "");
    }
    return out;
  }, [items, botFor, bots]);

  const chosen = selectedIds(items, ticks);
  const missingBot = instructionsWithoutBot(items, ticks, resolvedBot, botIds);
  const canRun = !!client && !busy && chosen.length > 0 && missingBot.length === 0;
  const appName = (app: ImportApp) => t(`importAssistants.apps.${app}`);

  const scan = async () => {
    if (!client || busy) return;
    setBusy("scan");
    setError(null);
    const input = buildScanInput(folder);
    try {
      const found = await client.scanImport(input);
      setScanned({ input, items: found.items });
      setTicks(initialTicks(found.items));
      setSecretTicks(initialSecretTicks());
      setBotFor({});
      setPhase("review");
    } catch (e) {
      setError(t("importAssistants.scanFailed", { message: humanLine(e) }));
    } finally {
      setBusy(null);
    }
  };

  const run = async () => {
    if (!client || !scanned || !canRun) return;
    setBusy("run");
    setError(null);
    try {
      const done = await client.runImport(buildRunInput(scanned.input, items, ticks, secretTicks));
      const finished = await applyInstructionResults(client, items, done.results, resolvedBot, bots, (b) => botView(b).name);
      setOutcomes(finished.outcomes);
      setPhase("done");
      if (finished.outcomes.some((o) => o.local === "persona_saved")) {
        await refreshBots().catch(() => {});
      }
    } catch (e) {
      setError(t("importAssistants.runFailed", { message: humanLine(e) }));
    } finally {
      setBusy(null);
    }
  };

  const titleKey = phase === "intro" ? "intro.title" : phase === "review" ? "review.title" : "done.title";
  const headingEl = (
    <h3 ref={heading} tabIndex={-1} className={styles.heading}>
      {t(`importAssistants.${titleKey}`)}
    </h3>
  );

  return (
    <div className={dialogStyles.root} data-testid="import-assistants">
      <DialogHeader
        title={t("importAssistants.title")}
        subtitle={t("importAssistants.subtitle")}
        onClose={() => closeGuard.current()}
        closeDisabled={busy !== null}
      />
      <DialogBody>
        {headingEl}
        {phase === "intro" ? (
          <>
            <p className={styles.lead}>{t("importAssistants.intro.reads")}</p>
            <ul className={styles.reads}>
              {IMPORT_NAMED_APPS.map((app) => (
                <li key={app}>{appName(app)}</li>
              ))}
            </ul>
            <Callout tone="info" title={t("importAssistants.intro.safeTitle")}>
              {t("importAssistants.intro.safe")}
            </Callout>
            <p className={styles.muted}>{t("importAssistants.intro.where")}</p>
            <Field
              label={t("importAssistants.intro.folderLabel")}
              htmlFor={`${uid}-folder`}
              hint={t("importAssistants.intro.folderHint")}
            >
              <input
                id={`${uid}-folder`}
                className={formStyles.input}
                value={folder}
                disabled={busy !== null}
                spellCheck={false}
                autoComplete="off"
                placeholder={t("importAssistants.intro.folderPlaceholder")}
                data-testid="import-folder"
                onChange={(e) => setFolder(e.target.value)}
              />
            </Field>
            {error ? (
              <p className={styles.error} role="alert" data-testid="import-error">
                {error}
              </p>
            ) : null}
          </>
        ) : null}

        {phase === "review" ? (
          <ReviewList
            groups={groups}
            itemsCount={items.length}
            ticks={ticks}
            secretTicks={secretTicks}
            resolvedBot={resolvedBot}
            bots={bots.map((b) => ({ id: b.id, name: botView(b).name }))}
            busy={busy !== null}
            appName={appName}
            onTick={(id, on) => setTicks((cur) => toggled(cur, id, on))}
            onSecret={(name, on) => setSecretTicks((cur) => toggled(cur, name, on))}
            onBot={(id, bot) => setBotFor((cur) => ({ ...cur, [id]: bot }))}
            error={error}
          />
        ) : null}

        {phase === "done" ? <Results items={items} outcomes={outcomes} /> : null}
      </DialogBody>

      {phase === "intro" ? (
        <DialogFooter>
          <Button variant="ghost" disabled={busy !== null} onClick={onClose}>
            {t("common.cancel")}
          </Button>
          <Button variant="primary" disabled={!client || busy !== null} data-testid="import-scan" onClick={() => void scan()}>
            {busy === "scan" ? <LoaderCircle aria-hidden="true" /> : <FolderInput aria-hidden="true" />}
            {busy === "scan" ? t("importAssistants.intro.looking") : t("importAssistants.intro.look")}
          </Button>
        </DialogFooter>
      ) : null}

      {phase === "review" ? (
        <DialogFooter
          note={
            missingBot.length > 0
              ? t("importAssistants.review.needBot")
              : t("importAssistants.review.selected", { count: chosen.length })
          }
          noteTone={missingBot.length > 0 ? "warning" : "info"}
        >
          <Button variant="ghost" disabled={busy !== null} onClick={() => { setError(null); setPhase("intro"); }}>
            {t("importAssistants.review.back")}
          </Button>
          {groups.mcp.length + groups.skills.length + groups.instructions.length > 0 ? (
            <Button variant="primary" disabled={!canRun} data-testid="import-run" onClick={() => void run()}>
              {busy === "run" ? <LoaderCircle aria-hidden="true" /> : null}
              {busy === "run"
                ? t("importAssistants.review.importing")
                : t("importAssistants.review.import", { count: chosen.length })}
            </Button>
          ) : (
            <Button variant="primary" onClick={onClose}>
              {t("common.close")}
            </Button>
          )}
        </DialogFooter>
      ) : null}

      {phase === "done" ? (
        <DialogFooter>
          <Button variant="ghost" onClick={() => { setError(null); setPhase("intro"); }}>
            {t("importAssistants.done.again")}
          </Button>
          <Button variant="primary" data-testid="import-done" onClick={onClose}>
            {t("common.close")}
          </Button>
        </DialogFooter>
      ) : null}
    </div>
  );
}

function ItemMeta({ item, appName }: { item: ImportItem; appName: (a: ImportApp) => string }) {
  const { t } = useTranslation();
  return (
    <>
      <span className={styles.itemName}>{item.name}</span>
      <span className={styles.itemMeta}>
        {t("importAssistants.item.from", { app: appName(item.app), source: item.source })}
      </span>
      {item.detail ? <span className={styles.itemDetail}>{item.detail}</span> : null}
    </>
  );
}

/** The reason an item cannot be imported (or was skipped), in the owner's language. */
function useReasonText() {
  const { t } = useTranslation();
  return (reason: string | null | undefined, fallback: string): string => {
    const known = knownReason(reason);
    return known ? t(`importAssistants.reason.${known}`) : fallback;
  };
}

function ReviewList({
  groups,
  itemsCount,
  ticks,
  secretTicks,
  resolvedBot,
  bots,
  busy,
  appName,
  onTick,
  onSecret,
  onBot,
  error
}: {
  groups: ReturnType<typeof groupItems>;
  itemsCount: number;
  ticks: ReadonlySet<string>;
  secretTicks: ReadonlySet<string>;
  resolvedBot: Readonly<Record<string, string>>;
  bots: { id: string; name: string }[];
  busy: boolean;
  appName: (a: ImportApp) => string;
  onTick: (id: string, on: boolean) => void;
  onSecret: (name: string, on: boolean) => void;
  onBot: (id: string, bot: string) => void;
  error: string | null;
}) {
  const { t } = useTranslation();
  const uid = useId();
  const reasonText = useReasonText();
  const importable = groups.mcp.length + groups.skills.length + groups.instructions.length;

  const row = (item: ImportItem) => {
    const on = ticks.has(item.id);
    const noBot = item.kind === "instructions" && bots.length === 0;
    return (
      <li key={item.id} className={styles.item} data-testid={`import-item-${item.id}`}>
        <label className={styles.itemMain}>
          <input
            type="checkbox"
            className={styles.check}
            checked={on}
            disabled={busy || noBot}
            onChange={(e) => onTick(item.id, e.target.checked)}
          />
          <span className={styles.itemText}>
            <ItemMeta item={item} appName={appName} />
          </span>
        </label>
        {item.kind === "mcp_server" ? <span className={styles.badge}>{t("importAssistants.item.off")}</span> : null}
        {on && item.secrets.length > 0 ? (
          <div className={styles.sub} role="group" aria-label={t("importAssistants.item.secretsGroup", { name: item.name })}>
            {item.secrets.map((slot) => (
              <label key={slot.name} className={styles.subRow}>
                <input
                  type="checkbox"
                  className={styles.check}
                  checked={secretTicks.has(slot.name)}
                  disabled={busy}
                  onChange={(e) => onSecret(slot.name, e.target.checked)}
                />
                <span>
                  {t("importAssistants.item.moveSecret", {
                    key: slot.key,
                    place: t(`importAssistants.item.place.${slot.place}`)
                  })}
                </span>
              </label>
            ))}
          </div>
        ) : null}
        {item.dropped.length > 0 ? (
          <p className={`${styles.muted} ${styles.sub}`}>
            {t("importAssistants.item.dropped", { names: item.dropped.join(", ") })}
          </p>
        ) : null}
        {item.kind === "instructions" ? (
          noBot ? (
            <p className={`${styles.muted} ${styles.sub}`}>{t("importAssistants.item.noBots")}</p>
          ) : on ? (
            <div className={`${formStyles.field} ${styles.sub}`}>
              <label className={formStyles.label} htmlFor={`${uid}-${item.id}`}>
                {t("importAssistants.item.botLabel")}
              </label>
              <select
                id={`${uid}-${item.id}`}
                className={`${formStyles.select} ${styles.select}`}
                value={resolvedBot[item.id] ?? ""}
                disabled={busy}
                onChange={(e) => onBot(item.id, e.target.value)}
              >
                <option value="" disabled>
                  {t("importAssistants.item.botPlaceholder")}
                </option>
                {bots.map((b) => (
                  <option key={b.id} value={b.id}>
                    {b.name}
                  </option>
                ))}
              </select>
            </div>
          ) : null
        ) : null}
      </li>
    );
  };

  const section = (key: "mcp" | "skills" | "instructions", list: ImportItem[]) =>
    list.length > 0 ? (
      <section className={styles.section} aria-labelledby={`${uid}-${key}`}>
        <h4 id={`${uid}-${key}`} className={styles.heading}>
          {t(`importAssistants.group.${key}`)}
        </h4>
        <p className={styles.muted}>{t(`importAssistants.group.${key}Note`)}</p>
        <ul className={styles.list}>{list.map(row)}</ul>
      </section>
    ) : null;

  if (itemsCount === 0) {
    return (
      <Callout tone="info" title={t("importAssistants.review.emptyTitle")} role="status">
        {t("importAssistants.review.emptyBody")}
      </Callout>
    );
  }

  return (
    <>
      {importable === 0 ? (
        <Callout tone="info" title={t("importAssistants.review.nothingTitle")} role="status">
          {t("importAssistants.review.nothingBody")}
        </Callout>
      ) : (
        <p className={styles.lead}>{t("importAssistants.review.lead")}</p>
      )}
      {section("mcp", groups.mcp)}
      {section("skills", groups.skills)}
      {section("instructions", groups.instructions)}
      {groups.notImported.length > 0 ? (
        <section className={styles.section} aria-labelledby={`${uid}-no`}>
          <h4 id={`${uid}-no`} className={styles.heading}>
            {t("importAssistants.group.notImported")}
          </h4>
          <ul className={styles.list}>
            {groups.notImported.map((item) => (
              <li key={item.id} className={styles.item} data-testid={`import-item-${item.id}`}>
                <div className={`${styles.itemMain} ${styles.itemMainStatic}`}>
                  <span className={styles.itemText}>
                    <ItemMeta item={item} appName={appName} />
                    <span className={styles.reason}>{reasonText(item.reason, item.note)}</span>
                  </span>
                </div>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
      {error ? (
        <p className={styles.error} role="alert" data-testid="import-error">
          {error}
        </p>
      ) : null}
    </>
  );
}

function Results({ items, outcomes }: { items: ImportItem[]; outcomes: ImportOutcome[] }) {
  const { t } = useTranslation();
  const reasonText = useReasonText();
  const counts = tally(outcomes);

  const message = (o: ImportOutcome, item: ImportItem | undefined): string => {
    if (o.local === "persona_saved") return t("importAssistants.result.personaSaved", { bot: o.botName ?? "" });
    if (o.local === "persona_too_long") {
      return t("importAssistants.result.personaTooLong", { bot: o.botName ?? "", max: PERSONA_MAX_CHARS });
    }
    if (o.local === "persona_failed") return t("importAssistants.result.personaFailed");
    if (knownReason(o.reason)) return reasonText(o.reason, o.message);
    if (o.status === "imported" && item) {
      if (item.kind === "mcp_server") return t("importAssistants.result.mcp");
      if (item.kind === "skill") return t("importAssistants.result.skill");
    }
    return o.message;
  };

  return (
    <>
      <p className={styles.lead} role="status" aria-live="polite" data-testid="import-summary">
        {t("importAssistants.done.summary", { ...counts })}
      </p>
      {importedServers(items, outcomes) ? (
        <Callout tone="warning" title={t("importAssistants.done.offTitle")}>
          {t("importAssistants.done.offBody")}
        </Callout>
      ) : null}
      <ul className={styles.list} data-testid="import-results">
        {outcomes.map((o) => {
          const item = items.find((i) => i.id === o.id);
          const Icon = o.status === "imported" ? CheckCircle2 : o.status === "skipped" ? CircleSlash : XCircle;
          const tone =
            o.status === "imported" ? styles.resultImported : o.status === "skipped" ? styles.resultSkipped : styles.resultFailed;
          return (
            <li key={o.id} className={styles.result} data-testid={`import-result-${o.id}`}>
              <Icon className={`${styles.resultIcon} ${tone}`} aria-hidden="true" />
              <div className={styles.resultBody}>
                <span className={styles.itemName}>
                  {item?.name ?? o.id}{" "}
                  <span className={`${styles.status} ${tone}`}>{t(`importAssistants.status.${o.status}`)}</span>
                </span>
                <span className={styles.reason}>{message(o, item)}</span>
                {o.secrets_stored.length > 0 ? (
                  <span className={styles.itemMeta}>
                    {t("importAssistants.result.stored", { names: o.secrets_stored.join(", ") })}
                  </span>
                ) : null}
                {o.secrets_left_out.length > 0 ? (
                  <span className={styles.itemMeta}>
                    {t("importAssistants.result.leftOut", { keys: o.secrets_left_out.join(", ") })}
                  </span>
                ) : null}
              </div>
            </li>
          );
        })}
      </ul>
    </>
  );
}
