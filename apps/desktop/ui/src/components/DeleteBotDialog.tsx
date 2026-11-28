import { ExternalLink, Trash2 } from "lucide-react";
import { type FormEvent, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { ApiError } from "../api/client";
import type { BotDeletePreview, BotDeleteReport } from "../api/types";
import {
  BACKUP_GUIDE_URL,
  type DeleteCount,
  type DeleteStep,
  deleteBlock,
  deletableInboxes,
  deleteOptions,
  deletedCounts,
  keptMailboxes,
  leftovers,
  nameMatches,
  reportCounts,
  STOPPED_MISSION_STATES,
  unfinishedRunIds
} from "../lib/botDelete";
import { openExternal } from "../lib/externalLink";
import { humanLine } from "../lib/humanError";
import { useDaemon } from "../state/DaemonContext";
import { Callout, DialogBody, DialogFooter, DialogHeader, Field } from "./DialogParts";
import { Modal } from "./Modal";
import { useToast } from "./Toast";
import dialog from "./Dialog.module.css";
import styles from "./DeleteBotDialog.module.css";

/** How long to wait for a stopped run to show as stopped before asking the daemon again. */
const STOP_POLL_MS = 400;
const STOP_POLLS = 8;

const wait = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Delete a bot for good, in two steps. Step 1 lists what goes (counts), what stays, the
 * optional inbox choice and the backup advice; step 2 asks for the bot's name and has the red
 * button. The daemon refuses while the bot is working or leads a waiting mission: the dialog
 * then offers to stop (or cancel) it. When something could not be cleaned up after the bot was
 * deleted, the toast says what and offers to try again.
 */
export function DeleteBotDialog({
  bot,
  open,
  onClose,
  onDeleted
}: {
  bot: { id: string; name: string };
  open: boolean;
  onClose: () => void;
  /** The bot is gone: the caller leaves the screens that showed it. */
  onDeleted: (report: BotDeleteReport) => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const { client, refreshBots, refreshChats, refreshRuns } = useDaemon();
  const [step, setStep] = useState<DeleteStep>("review");
  const [preview, setPreview] = useState<BotDeletePreview | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [alsoInbox, setAlsoInbox] = useState(false);
  const [typed, setTyped] = useState("");
  const [stopping, setStopping] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const nameInput = useRef<HTMLInputElement | null>(null);

  const load = useCallback(async (): Promise<BotDeletePreview | null> => {
    if (!client) return null;
    setLoadError(null);
    try {
      const next = await client.botDeletePreview(bot.id);
      setPreview(next);
      return next;
    } catch (e) {
      setLoadError(humanLine(e));
      return null;
    }
  }, [client, bot.id]);

  // Every opening starts at step 1 with a fresh look at what the bot owns.
  useEffect(() => {
    if (!open) return;
    setStep("review");
    setPreview(null);
    setAlsoInbox(false);
    setTyped("");
    setError(null);
    void load();
  }, [open, load]);

  useEffect(() => {
    if (step === "confirm") nameInput.current?.focus();
  }, [step]);

  const block = preview ? deleteBlock(preview) : null;
  const inboxes = preview ? deletableInboxes(preview) : [];
  const busy = stopping || deleting;
  // The name the daemon just reported: the one to type, even if the screen behind kept an old one.
  const name = preview?.name ?? bot.name;

  /** Back to step 1: the typed name goes, so a changed choice there is confirmed again. */
  const back = () => {
    setTyped("");
    setError(null);
    setStep("review");
  };

  /** Stop what the bot is doing (its unfinished runs, its running or waiting missions), then look again. */
  const stopIt = async () => {
    if (!client || busy) return;
    setStopping(true);
    setError(null);
    try {
      const runs = unfinishedRunIds(await client.listRuns(), bot.id);
      await Promise.all(runs.map((id) => client.cancelRun(id).catch(() => undefined)));
      const missions = await client.listMissions(bot.id, "active");
      await Promise.all(
        missions.filter((m) => STOPPED_MISSION_STATES.has(m.state)).map((m) => client.steerMission(m.id, "cancel").catch(() => undefined))
      );
      for (let i = 0; i < STOP_POLLS; i++) {
        const next = await load();
        if (!next || deleteBlock(next) === null) break;
        await wait(STOP_POLL_MS);
      }
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setStopping(false);
    }
  };

  const items = (rows: DeleteCount[]) => rows.map((r) => `${t(`botDelete.item.${r.key}`)}: ${r.count}`).join(", ");

  /** Say what was removed; when something was left over, say what and offer the same request again. */
  const announce = (report: BotDeleteReport, opts: { deleteInbox: boolean }) => {
    const parts = [items(reportCounts(report))].filter(Boolean);
    if (report.inboxes_deleted.length > 0) {
      parts.push(t("botDelete.doneInbox", { list: report.inboxes_deleted.join(", ") }));
    }
    const left = leftovers(report);
    if (left.steps.length === 0 && left.inboxes.length === 0) {
      toast.show(
        parts.length > 0
          ? t("botDelete.done", { name: report.name, items: parts.join(". ") })
          : t("botDelete.doneEmpty", { name: report.name }),
        "success"
      );
      return;
    }
    const problems: string[] = [];
    if (left.steps.length > 0) {
      problems.push(t("botDelete.leftover.steps", { list: left.steps.map((k) => t(`botDelete.step.${k}`)).join(", ") }));
    }
    if (left.inboxes.length > 0) {
      problems.push(t("botDelete.leftover.inbox", { list: left.inboxes.join(", ") }));
    }
    const retryable = left.steps.length > 0;
    toast.show(t("botDelete.leftover.text", { name: report.name, problems: problems.join(" ") }), "danger", {
      durationMs: 30000,
      action: retryable
        ? {
            label: t("botDelete.retry"),
            onClick: () => {
              if (!client) return;
              client.deleteBot(bot.id, opts).then(
                (again) => announce(again, opts),
                (e: unknown) => toast.show(humanLine(e), "danger")
              );
            }
          }
        : undefined
    });
  };

  const remove = async (event?: FormEvent) => {
    event?.preventDefault();
    if (!client || !preview || deleting || !nameMatches(typed, name)) return;
    setDeleting(true);
    setError(null);
    try {
      const opts = deleteOptions(preview, alsoInbox);
      const report = await client.deleteBot(bot.id, opts);
      // Its chats and runs left with it: the sidebar and the run lists must not keep showing them.
      await Promise.all([refreshBots(), refreshChats(), refreshRuns()]).catch(() => {});
      announce(report, opts);
      onDeleted(report);
    } catch (e) {
      setDeleting(false);
      if (e instanceof ApiError && e.status === 409) {
        // It started working since step 1: look again and say so on step 1.
        setStep("review");
        setTyped("");
        await load();
      }
      setError(humanLine(e));
    }
  };

  const mailboxLine = (m: BotDeletePreview["mailboxes"][number]) =>
    t(`botDelete.mailbox.${m.fate}`, { address: m.address });

  const review = (
    <>
      <DialogBody testId="delete-bot-review">
        {loadError ? (
          <Callout tone="danger" title={t("botDelete.loadFailed")} role="alert">
            <p className={styles.line}>{loadError}</p>
            <Button size="sm" variant="secondary" onClick={() => void load()}>
              {t("botDelete.retry")}
            </Button>
          </Callout>
        ) : !preview ? (
          <p className={dialog.hint}>{t("botDelete.loading")}</p>
        ) : (
          <>
            {block ? (
              <Callout tone="warning" title={t("botDelete.working.title")} role="alert">
                <p className={styles.line} data-testid="delete-bot-working">
                  {t(`botDelete.working.${block}`, { name })}
                </p>
                <Button size="sm" variant="secondary" disabled={busy} data-testid="delete-bot-stop" onClick={() => void stopIt()}>
                  {block === "waiting"
                    ? stopping
                      ? t("botDelete.working.cancelling")
                      : t("botDelete.working.cancelWaiting")
                    : stopping
                      ? t("botDelete.working.stopping")
                      : t("botDelete.working.stop")}
                </Button>
              </Callout>
            ) : null}
            <section className={dialog.block} aria-labelledby="delete-bot-goes">
              <h3 id="delete-bot-goes" className={dialog.blockTitle}>
                {t("botDelete.goes.title")}
              </h3>
              <ul className={styles.counts} data-testid="delete-bot-counts">
                {deletedCounts(preview).map((row) => (
                  <li key={row.key} className={styles.count}>
                    <span>{t(`botDelete.item.${row.key}`)}</span>
                    <strong data-testid={`delete-bot-count-${row.key}`}>{row.count}</strong>
                  </li>
                ))}
              </ul>
              <ul className={styles.list}>
                <li>{t("botDelete.goes.settings")}</li>
                {preview.mailboxes
                  .filter((m) => m.fate === "removed")
                  .map((m) => (
                    <li key={m.name}>{mailboxLine(m)}</li>
                  ))}
              </ul>
            </section>
            <section className={dialog.block} aria-labelledby="delete-bot-stays">
              <h3 id="delete-bot-stays" className={dialog.blockTitle}>
                {t("botDelete.stays.title")}
              </h3>
              <ul className={styles.list} data-testid="delete-bot-stays">
                <li>
                  {preview.workspace_folder
                    ? t("botDelete.stays.filesWithFolder", { folder: preview.workspace_folder })
                    : t("botDelete.stays.files")}
                </li>
                <li>{t("botDelete.stays.shared")}</li>
                {keptMailboxes(preview).map((m) => (
                  <li key={m.name}>{mailboxLine(m)}</li>
                ))}
                <li>{t("botDelete.stays.audit")}</li>
              </ul>
            </section>
            {inboxes.length > 0 ? (
              <label className={styles.inbox}>
                <input
                  type="checkbox"
                  checked={alsoInbox}
                  data-testid="delete-bot-inbox"
                  onChange={(e) => setAlsoInbox(e.target.checked)}
                />
                <span>
                  <span className={styles.inboxLabel}>
                    {t("botDelete.inbox.label", { address: inboxes.map((m) => m.address).join(", ") })}
                  </span>
                  <span className={dialog.hint}>{t("botDelete.inbox.hint")}</span>
                </span>
              </label>
            ) : null}
            <Callout tone="info" title={t("botDelete.backup.title")}>
              <p className={styles.line}>{t("botDelete.backup.body")}</p>
              <Button size="sm" variant="ghost" data-testid="delete-bot-backup" onClick={() => openExternal(BACKUP_GUIDE_URL)}>
                <ExternalLink aria-hidden="true" />
                {t("botDelete.backup.link")}
              </Button>
            </Callout>
            {error ? (
              <p className={dialog.error} role="alert">
                {error}
              </p>
            ) : null}
          </>
        )}
      </DialogBody>
      <DialogFooter landmark={false}>
        <Button variant="ghost" disabled={busy} data-autofocus onClick={onClose}>
          {t("common.cancel")}
        </Button>
        <Button
          variant="secondary"
          disabled={!preview || block !== null || busy}
          data-testid="delete-bot-continue"
          onClick={() => setStep("confirm")}
        >
          {t("botDelete.continue")}
        </Button>
      </DialogFooter>
    </>
  );

  const confirm = (
    <form onSubmit={(e) => void remove(e)} className={styles.form}>
      <DialogBody testId="delete-bot-confirm">
        <Callout tone="danger" title={t("botDelete.confirm.warnTitle")}>
          <p className={styles.line}>
            {t("botDelete.confirm.warn", { name, items: preview ? items(deletedCounts(preview)) || t("botDelete.confirm.nothingElse") : "" })}
          </p>
          {alsoInbox && inboxes.length > 0 ? (
            <p className={styles.line}>{t("botDelete.confirm.inbox", { address: inboxes.map((m) => m.address).join(", ") })}</p>
          ) : null}
        </Callout>
        <Field label={t("botDelete.confirm.label", { name })} htmlFor="delete-bot-name" hint={t("botDelete.confirm.hint")}>
          <input
            id="delete-bot-name"
            ref={nameInput}
            className={dialog.input}
            value={typed}
            autoComplete="off"
            spellCheck={false}
            disabled={deleting}
            data-testid="delete-bot-name"
            onChange={(e) => setTyped(e.target.value)}
          />
        </Field>
        {error ? (
          <p className={dialog.error} role="alert">
            {error}
          </p>
        ) : null}
      </DialogBody>
      <DialogFooter landmark={false}>
        <Button type="button" variant="ghost" disabled={deleting} data-testid="delete-bot-back" onClick={back}>
          {t("botDelete.back")}
        </Button>
        <Button
          type="submit"
          variant="danger"
          disabled={deleting || !nameMatches(typed, name)}
          data-testid="delete-bot-confirm-button"
        >
          <Trash2 aria-hidden="true" />
          {deleting ? t("botDelete.deleting") : t("botDelete.confirm.button")}
        </Button>
      </DialogFooter>
    </form>
  );

  const title = step === "review" ? t("botDelete.title", { name }) : t("botDelete.confirm.title", { name });
  return (
    <Modal open={open} onClose={busy ? () => {} : onClose} size="md" label={title}>
      {open ? (
        <div className={dialog.root} data-testid="delete-bot-dialog" data-step={step}>
          <DialogHeader
            title={title}
            subtitle={step === "review" ? t("botDelete.subtitle") : t("botDelete.confirm.subtitle")}
            onClose={onClose}
            closeDisabled={busy}
            landmark={false}
          />
          {step === "review" ? review : confirm}
        </div>
      ) : null}
    </Modal>
  );
}
