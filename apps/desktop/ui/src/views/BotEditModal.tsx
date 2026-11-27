import {
  AppWindow,
  BookOpenText,
  BrainCircuit,
  CircleCheck,
  Download,
  Gauge,
  MessageSquareText,
  MonitorSmartphone,
  Plus,
  ShieldCheck,
  SlidersHorizontal,
  UserRound
} from "lucide-react";
import {
  type MutableRefObject,
  type ReactNode,
  useCallback,
  useEffect,
  useId,
  useRef,
  useState
} from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, Button } from "ui";
import type { Bot } from "../api/types";
import { useBotFormDeps } from "../components/BotAdvancedFields";
import {
  AdvancedFields,
  AppFields,
  IdentityFields,
  ModelFields,
  PermissionFields,
  PlaceFields
} from "../components/BotEditorFields";
import { BotInstructionsField } from "../components/BotInstructionsField";
import { BotMemorySettings } from "../components/BotMemorySettings";
import {
  Callout,
  ConfirmDialog,
  DialogFooter,
  DialogBody,
  DialogHeader,
  dialogStyles
} from "../components/DialogParts";
import { BotLimitsField } from "../components/BotLimitsField";
import { BotVoiceField } from "../components/BotVoiceField";
import { BotEngineBlock } from "../components/BotEngineBlock";
import { Modal } from "../components/Modal";
import { useEngineConsent } from "../state/EngineConsent";
import { SkillsEditor } from "../components/SkillsEditor";
import { useToast } from "../components/Toast";
import {
  BOT_EDITOR_SECTIONS,
  type BotEditorSection,
  closeAction,
  deleteBlock,
  dirtySections,
  editorProblems,
  SEPARATE_SAVE,
  sectionAtScroll
} from "../lib/botEditor";
import { botChats } from "../lib/botLanding";
import { type BotDraft, draftFromBot, formatModelRef, runtimeTargetLabel } from "../lib/botProfile";
import { performEditorSave, type ProfilePart } from "../lib/botSave";
import { buildPutSkillsPayload, isBotSkillsDirty } from "../lib/botSkills";
import { useDaemon } from "../state/DaemonContext";
import { useLeaveGuard } from "../state/LeaveGuard";
import { type BotSkillsState, useSkillsDraft } from "../state/useBotSkills";
import { useCustomText } from "../state/useCustomText";
import styles from "./BotEditModal.module.css";
import { humanLine } from "../lib/humanError";
import { useServiceState } from "../state/ServiceState";
import { saveTextFile } from "../lib/profileShare";
import { HomeHint } from "../components/HomeHint";

const SECTION_ICONS: Record<BotEditorSection, typeof UserRound> = {
  identity: UserRound,
  instructions: MessageSquareText,
  models: BrainCircuit,
  place: MonitorSmartphone,
  permissions: ShieldCheck,
  apps: AppWindow,
  memory: BookOpenText,
  limits: Gauge,
  advanced: SlidersHorizontal
};

/** Where `el` starts inside the scrolling `body`, in the body's scroll coordinates. */
function topIn(body: HTMLElement, el: HTMLElement): number {
  return el.getBoundingClientRect().top - body.getBoundingClientRect().top + body.scrollTop;
}

/**
 * "Edit bot" as one page (owner feedback 244, point 5; redesign ux5): every section stacked in
 * one scrolling body, a jump list on the left that follows the scroll, and a footer that always
 * says whether something is unsaved.  Memory and spending limits do not wait for Save, and say so.
 */
export function BotEditModal({
  open,
  bot,
  skills,
  initialSection,
  onClose,
  onDeleted,
  onAddSkill
}: {
  open: boolean;
  bot: Bot | undefined;
  skills: BotSkillsState;
  initialSection?: BotEditorSection;
  onClose: () => void;
  onDeleted: (botId: string) => void;
  onAddSkill: () => void;
}) {
  const { t } = useTranslation();
  const closeGuard = useRef<() => void>(onClose);
  const shown = open && bot !== undefined;
  return (
    <Modal open={shown} onClose={() => closeGuard.current()} label={t("botForm.editTitle")} size="lg">
      {shown && bot ? (
        <BotEditDialog
          key={bot.id}
          bot={bot}
          skills={skills}
          initialSection={initialSection}
          closeGuard={closeGuard}
          onClose={onClose}
          onDeleted={onDeleted}
          onAddSkill={onAddSkill}
        />
      ) : null}
    </Modal>
  );
}

function BotEditDialog({
  bot,
  skills,
  initialSection,
  closeGuard,
  onClose,
  onDeleted,
  onAddSkill
}: {
  bot: Bot;
  skills: BotSkillsState;
  initialSection?: BotEditorSection;
  /** What the Modal calls on Esc / backdrop press. */
  closeGuard: MutableRefObject<() => void>;
  onClose: () => void;
  onDeleted: (botId: string) => void;
  onAddSkill: () => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const uid = useId();
  const { client, bots, chats, refreshBots } = useDaemon();
  const { askEngine, applyEngine } = useEngineConsent();
  const { providersDto, servers, setServers, allowed, setAllowed, initialApps, setInitialApps } =
    useBotFormDeps(client, bot.id, true);

  // The draft the dialog opened with: later refreshes of the bot list must not make an
  // untouched form look edited (or silently replace what the user typed).
  const [savedDraft, setSavedDraft] = useState<BotDraft>(() => draftFromBot(bot));
  const [draft, setDraft] = useState<BotDraft>(savedDraft);
  // The stored bot the dialog was opened on: the conflict check compares the daemon's copy with it.
  const [baseline, setBaseline] = useState<Bot>(bot);
  // Hand-typed model ids as typed (Advanced); kept here so they survive a section change.
  const { customText, setCustom, forgetFor, reset: resetCustom } = useCustomText(setDraft);
  const [pendingFallback, setPendingFallback] = useState("");
  // Runs of this bot the daemon still holds (it refuses to delete a bot with any); `null` until known.
  const [runCount, setRunCount] = useState<number | null>(null);
  useEffect(() => {
    if (!client) return undefined;
    let cancelled = false;
    client
      .listRuns()
      .then((runs) => {
        if (!cancelled) setRunCount(runs.filter((r) => r.bot_id === bot.id).length);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [client, bot.id]);
  const [conflict, setConflict] = useState<{ parts: ProfilePart[]; fresh: Bot } | null>(null);
  const pendingLeave = useRef<(() => void) | null>(null);
  // The section in view (the jump list highlights it); it follows the scroll.
  const [section, setSection] = useState<BotEditorSection>(initialSection ?? "identity");
  const [skillsDraft, setSkillsDraft] = useSkillsDraft(skills.data);
  const [saving, setSaving] = useState(false);
  // While the local service is down only Save / Export wait; the typed input stays as it is.
  const serviceBlocked = useServiceState().blocked;
  const wasBlocked = useRef(false);
  useEffect(() => {
    if (serviceBlocked) {
      wasBlocked.current = true;
    } else if (wasBlocked.current) {
      wasBlocked.current = false;
      setError(null); // the "service not answering" line is stale once it is back
    }
  }, [serviceBlocked]);
  const [error, setError] = useState<string | null>(null);
  const [confirmClose, setConfirmClose] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const sectionRefs = useRef<Partial<Record<BotEditorSection, HTMLElement | null>>>({});

  // The file is built by the daemon from the STORED bot (an allow-list: no secrets, grants,
  // memory, chats, folders or devices), so unsaved edits are not part of it.
  const exportProfile = async () => {
    if (!client || exporting) return;
    setExporting(true);
    setError(null);
    try {
      const out = await client.exportProfile(bot.id, false);
      saveTextFile(out.filename, out.contents);
      toast.show(t("profileShare.exported", { name: bot.name }), "success");
    } catch (e) {
      setError(humanLine(e));
    } finally {
      setExporting(false);
    }
  };

  const patch = (p: Partial<BotDraft>) => {
    setDraft((d) => ({ ...d, ...p }));
    forgetFor(p);
  };

  const skillsDirty = Boolean(
    skills.data && skillsDraft && isBotSkillsDirty(skillsDraft, skills.data)
  );
  const dirtyIn = dirtySections({
    draft,
    initialDraft: savedDraft,
    apps: allowed,
    initialApps,
    skillsDirty,
    customText,
    pendingFallback
  });
  const dirty = dirtyIn.length > 0;
  const problems = editorProblems(draft, customText, pendingFallback);
  const nameMissing = problems.some((p) => p.problem === "nameMissing");
  const executorMissing = problems.some((p) => p.problem === "executorMissing");

  const requestClose = () => {
    const action = closeAction(dirty, saving || deleting);
    if (action === "close") onClose();
    else if (action === "confirm") {
      pendingLeave.current = null;
      setConfirmClose(true);
    }
  };
  useEffect(() => {
    closeGuard.current = requestClose;
  });
  // Route changes (another bot, a chat, the palette, the panel) ask the same question.
  useLeaveGuard({
    state: () => ({ dirty, busy: saving || deleting }),
    confirm: (proceed) => {
      pendingLeave.current = proceed;
      setConfirmClose(true);
    }
  });

  // After a jump the chosen section stays highlighted (a short section near the end may never
  // reach the top); the reader's own scrolling hands the highlight back to the scroll position.
  const jumped = useRef(false);

  /** Scroll the page to a section and put the keyboard focus on its title. */
  const goTo = useCallback((next: BotEditorSection, smooth = true) => {
    const body = bodyRef.current;
    const el = sectionRefs.current[next];
    setSection(next);
    if (!body || !el) return;
    jumped.current = true;
    const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches === true;
    body.scrollTo({ top: Math.max(0, topIn(body, el) - 8), behavior: smooth && !reduced ? "smooth" : "auto" });
    el.querySelector<HTMLElement>("h3")?.focus({ preventScroll: true });
  }, []);

  // Opened on a section ("Raise limit" opens Limits): start there.
  const opened = useRef(initialSection);
  useEffect(() => {
    if (opened.current) goTo(opened.current, false);
  }, [goTo]);

  // The jump list follows the scroll.
  useEffect(() => {
    const body = bodyRef.current;
    if (!body) return undefined;
    let frame = 0;
    const onScroll = () => {
      if (jumped.current) return;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        const els = BOT_EDITOR_SECTIONS.map((s) => sectionRefs.current[s]);
        if (els.some((el) => !el)) return;
        const tops = els.map((el) => topIn(body, el as HTMLElement));
        const at = sectionAtScroll(tops, {
          top: body.scrollTop,
          height: body.clientHeight,
          scrollHeight: body.scrollHeight
        });
        setSection(BOT_EDITOR_SECTIONS[at] ?? "identity");
      });
    };
    const release = () => {
      jumped.current = false;
    };
    const userScroll = ["wheel", "touchstart", "pointerdown", "keydown"] as const;
    body.addEventListener("scroll", onScroll, { passive: true });
    for (const e of userScroll) body.addEventListener(e, release, { passive: true });
    return () => {
      cancelAnimationFrame(frame);
      body.removeEventListener("scroll", onScroll);
      for (const e of userScroll) body.removeEventListener(e, release);
    };
  }, []);

  /** All of Save lives in `performEditorSave`; this only collects state and shows the outcome. */
  const save = async (expectedVersion?: number) => {
    if (!client || saving) return;
    setSaving(true);
    setError(null);
    // A CLI model that only works with its own tools: the one consent step, before anything is saved.
    const answer = await askEngine(formatModelRef(draft.executor), baseline.config);
    if (answer === null) {
      setSaving(false);
      return;
    }
    const out = await performEditorSave({
      client,
      baseline,
      draft,
      openedDraft: savedDraft,
      problems,
      initialApps,
      allowed,
      allBotIds: bots.map((b) => b.id),
      skills: {
        payload:
          skillsDirty && skills.data && skillsDraft
            ? buildPutSkillsPayload(skillsDraft, skills.data.available)
            : null,
        save: async (payload) => {
          const r = await skills.save(payload);
          return r.ok ? { ok: true } : { ok: false, failure: r.failure, message: r.message };
        }
      },
      expectedVersion
    });
    try {
      if (out.kind === "blocked") {
        goTo(out.section);
        if (out.message) setError(t(out.message.key, out.message.params));
      } else if (out.kind === "conflict") {
        setConflict({ parts: out.parts, fresh: out.fresh });
      } else if (out.kind === "failed") {
        setError(out.message);
      } else {
        setConflict(null);
        await applyEngine(out.bot.id, answer).catch((e) => setError(humanLine(e)));
        if (out.profileWritten) {
          const next = draftFromBot(out.bot);
          setBaseline(out.bot);
          setSavedDraft(next);
          setDraft(next);
          resetCustom();
          setPendingFallback("");
          await refreshBots().catch(() => {});
        }
        if (out.servers) setServers(out.servers);
        setAllowed(out.allowed);
        setInitialApps(out.initialApps);
        if (out.close) {
          toast.show(t("botEdit.saved", { name: out.bot.name }), "success");
          onClose();
        } else {
          setError(out.messages.map((m) => t(m.key, m.params)).join(" "));
        }
      }
    } finally {
      setSaving(false);
    }
  };

  /** Drop the profile edits and show what is stored now; skills and app edits stay. */
  const reloadProfile = () => {
    if (!conflict) return;
    const next = draftFromBot(conflict.fresh);
    setBaseline(conflict.fresh);
    setSavedDraft(next);
    setDraft(next);
    resetCustom();
    setPendingFallback("");
    setConflict(null);
  };

  const remove = async () => {
    if (!client || deleting) return;
    setDeleting(true);
    setDeleteError(null);
    try {
      await client.deleteBot(bot.id);
      await refreshBots().catch(() => {});
      toast.show(t("botEdit.deleted", { name: bot.name }), "info");
      onDeleted(bot.id);
    } catch (e) {
      setDeleteError(humanLine(e));
      setDeleting(false);
    }
  };

  const chatCount = botChats(chats, bot.id).length;
  const deleteBlocked = deleteBlock(chatCount, runCount);
  const headId = (s: BotEditorSection) => `${uid}-section-${s}`;

  /** One section of the page: icon, title, one line of what it is for, then its fields. */
  const block = (s: BotEditorSection, children: ReactNode) => {
    const Icon = SECTION_ICONS[s];
    return (
      <section
        key={s}
        ref={(el) => {
          sectionRefs.current[s] = el;
        }}
        className={styles.section}
        aria-labelledby={headId(s)}
        data-testid={`bot-edit-section-${s}`}
      >
        <div className={styles.sectionHead}>
          <span className={styles.sectionIcon} aria-hidden="true">
            <Icon />
          </span>
          <div className={styles.sectionText}>
            <h3 id={headId(s)} className={styles.sectionTitle} tabIndex={-1}>
              {t(`botEdit.section.${s}`)}
            </h3>
            <p className={styles.sectionHint}>{t(`botEdit.sectionHint.${s}`)}</p>
          </div>
          {SEPARATE_SAVE[s] ? (
            <span className={styles.instant} data-testid={`bot-edit-instant-${s}`}>
              <CircleCheck aria-hidden="true" />
              {SEPARATE_SAVE[s] === "onChange" ? t("botEdit.savedOnChange") : t("botEdit.savedOwnButton")}
            </span>
          ) : dirtyIn.includes(s) ? (
            <span className={styles.changed}>
              <i className={dialogStyles.dirtyDot} aria-hidden="true" />
              {t("botEdit.unsaved")}
            </span>
          ) : null}
        </div>
        {children}
      </section>
    );
  };

  // The footer always says where the edits stand: nothing to save, what is unsaved, what blocks Save.
  const footerNote = saving ? (
    t("botEdit.saving")
  ) : problems[0] ? (
    t(`botEdit.problem.${problems[0].problem}`)
  ) : dirty ? (
    <>
      <i className={dialogStyles.dirtyDot} aria-hidden="true" />
      {t("botEdit.unsavedIn", { sections: dirtyIn.map((s) => t(`botEdit.section.${s}`)).join(", ") })}
    </>
  ) : (
    <>
      <CircleCheck className={styles.cleanIcon} aria-hidden="true" />
      {t("botEdit.allSaved")}
    </>
  );

  return (
    <div className={styles.shell} data-testid="bot-edit-dialog">
      <nav className={styles.nav} aria-label={t("botEdit.sections")}>
        <div className={styles.navBot}>
          <BotAvatar
            bot={{
              id: bot.id,
              name: draft.name,
              shape: draft.shape,
              color: draft.color,
              avatar: draft.avatar
            }}
            size="sm"
            decorative
          />
          <span className={styles.navBotName}>{draft.name.trim() || bot.name}</span>
        </div>
        <ul className={styles.tabs} inert={saving || deleting}>
          {BOT_EDITOR_SECTIONS.map((s) => {
            const Icon = SECTION_ICONS[s];
            const active = s === section;
            const blocked = problems.some((p) => p.section === s);
            const changed = dirtyIn.includes(s);
            return (
              <li key={s}>
                <button
                  type="button"
                  aria-current={active ? "location" : undefined}
                  data-testid={`bot-edit-tab-${s}`}
                  className={[styles.tab, active ? styles.tabActive : ""].filter(Boolean).join(" ")}
                  onClick={() => goTo(s)}
                >
                  <Icon aria-hidden="true" />
                  <span className={styles.tabLabel}>{t(`botEdit.section.${s}`)}</span>
                  {blocked ? (
                    <span className={`${styles.mark} ${styles.markProblem}`} title={t("botEdit.needsAttention")}>
                      <span className={dialogStyles.visuallyHidden}>{t("botEdit.needsAttention")}</span>
                    </span>
                  ) : changed ? (
                    <span className={styles.mark} title={t("botEdit.unsaved")}>
                      <span className={dialogStyles.visuallyHidden}>{t("botEdit.unsaved")}</span>
                    </span>
                  ) : null}
                </button>
              </li>
            );
          })}
        </ul>
      </nav>

      <div className={dialogStyles.root}>
        <DialogHeader
          title={t("botForm.editTitle")}
          subtitle={t("botEdit.pageHint")}
          closeDisabled={saving || deleting}
          onClose={requestClose}
          actions={<HomeHint />}
        />
        <DialogBody scrollRef={bodyRef} inert={saving || deleting} testId="bot-edit-page">
          {block(
            "identity",
            <>
              <IdentityFields botId={bot.id} draft={draft} patch={patch} nameMissing={nameMissing} />
              <BotVoiceField botId={bot.id} />
            </>
          )}
          {block(
            "instructions",
            <BotInstructionsField value={draft.persona} onChange={(persona) => patch({ persona })} />
          )}
          {block(
            "models",
            <>
              <ModelFields
                draft={draft}
                patch={patch}
                providersDto={providersDto}
                executorMissing={executorMissing}
                pendingFallback={pendingFallback}
                onPendingFallback={setPendingFallback}
              />
              <h4 className={styles.subTitle}>{t("botForm.engine.title")}</h4>
              <BotEngineBlock bot={baseline} />
            </>
          )}
          {block(
            "place",
            <PlaceFields
              computerTarget={runtimeTargetLabel(baseline.config, "computer")}
              browserTarget={runtimeTargetLabel(baseline.config, "browser")}
            />
          )}
          {block("permissions", <PermissionFields draft={draft} patch={patch} />)}
          {block(
            "apps",
            <>
              <AppFields
                servers={servers}
                allowed={allowed}
                onToggle={(name, checked) => setAllowed((a) => ({ ...a, [name]: checked }))}
              />
              <div className={styles.subHead}>
                <div>
                  <h4 className={styles.subTitle}>{t("botEdit.skillsTitle")}</h4>
                  <p className={styles.sectionHint}>{t("botEdit.skillsHint")}</p>
                </div>
                <Button size="sm" variant="secondary" onClick={onAddSkill}>
                  <Plus aria-hidden="true" />
                  {t("addSkill.open")}
                </Button>
              </div>
              {skills.error && !skills.data ? (
                <Callout tone="danger" title={t("bots.skills.loadFailed")} role="alert">
                  {skills.error}
                </Callout>
              ) : !skills.data || !skillsDraft ? (
                <p className={dialogStyles.hint}>{t("bots.skills.loading")}</p>
              ) : (
                <SkillsEditor data={skills.data} draft={skillsDraft} onChange={setSkillsDraft} />
              )}
            </>
          )}
          {block(
            "memory",
            <div className={styles.memory}>
              <BotMemorySettings botId={bot.id} />
            </div>
          )}
          {block("limits", <BotLimitsField botId={bot.id} />)}
          {block(
            "advanced",
            <>
              <div className={styles.shareRow}>
                <div>
                  <p className={styles.shareTitle}>{t("profileShare.export")}</p>
                  <p className={dialogStyles.hint}>
                    {dirty ? t("profileShare.exportSaveFirst") : t("profileShare.exportHint")}
                  </p>
                </div>
                <Button
                  variant="secondary"
                  disabled={saving || deleting || exporting || dirty || serviceBlocked}
                  data-testid="bot-edit-export"
                  onClick={() => void exportProfile()}
                >
                  <Download aria-hidden="true" />
                  {t("profileShare.export")}
                </Button>
              </div>
              <AdvancedFields
                draft={draft}
                customText={customText}
                onCustomText={setCustom}
                deleteBlocked={deleteBlocked}
                chatCount={chatCount}
                runCount={runCount ?? 0}
                onDelete={() => {
                  setDeleteError(null);
                  setConfirmDelete(true);
                }}
              />
            </>
          )}
        </DialogBody>
        {/* Problems with Save sit right above the footer, never at the end of a long page. */}
        {error || conflict ? (
          <div className={styles.footerAlert}>
            {conflict ? (
            <Callout tone="warning" title={t("botEdit.conflict.title")} role="alert">
              <p>
                {t("botEdit.conflict.body", {
                  parts: conflict.parts.map((p) => t(`botEdit.conflict.part.${p}`)).join(", ")
                })}
              </p>
              <div className={styles.noticeActions}>
                <Button size="sm" variant="secondary" data-testid="bot-edit-reload" onClick={reloadProfile}>
                  {t("botEdit.conflict.reload")}
                </Button>
                <Button
                  size="sm"
                  variant="secondary"
                  data-testid="bot-edit-overwrite"
                  disabled={saving}
                  onClick={() => void save(conflict.fresh.version)}
                >
                  {t("botEdit.conflict.overwrite")}
                </Button>
              </div>
            </Callout>
            ) : null}
            {error ? (
              <Callout tone="danger" role="alert">
                {error}
              </Callout>
            ) : null}
          </div>
        ) : null}
        <DialogFooter noteTone={problems.length > 0 && !saving ? "warning" : "info"} note={footerNote}>
          <Button variant="ghost" disabled={saving || deleting} onClick={requestClose}>
            {t("common.cancel")}
          </Button>
          <Button
            variant="primary"
            disabled={saving || deleting || !dirty || serviceBlocked}
            data-testid="bot-edit-save"
            onClick={() => void save()}
          >
            {saving ? t("botEdit.saving") : t("common.save")}
          </Button>
        </DialogFooter>
      </div>

      <ConfirmDialog
        open={confirmClose}
        title={t("botEdit.discardTitle")}
        body={t("botEdit.discardBody")}
        confirmLabel={t("botEdit.discard")}
        cancelLabel={t("botEdit.keepEditing")}
        tone="danger"
        onCancel={() => {
          pendingLeave.current = null;
          setConfirmClose(false);
        }}
        onConfirm={() => {
          const proceed = pendingLeave.current;
          pendingLeave.current = null;
          setConfirmClose(false);
          onClose();
          proceed?.();
        }}
      />
      <ConfirmDialog
        open={confirmDelete}
        title={t("botEdit.deleteConfirmTitle", { name: bot.name })}
        body={t("botEdit.deleteConfirmBody")}
        confirmLabel={t("botEdit.delete")}
        tone="danger"
        busy={deleting}
        error={deleteError}
        onCancel={() => setConfirmDelete(false)}
        onConfirm={() => void remove()}
      />
    </div>
  );
}
