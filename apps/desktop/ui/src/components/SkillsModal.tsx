import { Library, Plus } from "lucide-react";
import { type MutableRefObject, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import { closeAction } from "../lib/botEditor";
import {
  MAX_ASSIGNED_SKILLS,
  buildPutSkillsPayload,
  exceedsSkillLimit,
  isBotSkillsDirty
} from "../lib/botSkills";
import { useLeaveGuard } from "../state/LeaveGuard";
import { type BotSkillsState, useSkillsDraft } from "../state/useBotSkills";
import { Callout, ConfirmDialog, DialogFooter, DialogHeader, dialogStyles } from "./DialogParts";
import { Modal } from "./Modal";
import { SkillsEditor } from "./SkillsEditor";
import { useToast } from "./Toast";

/** "Skills" of one bot as a dialog, opened from the right panel. */
export function SkillsModal({
  open,
  botName,
  skills,
  onClose,
  onAddSkill,
  onBrowse
}: {
  open: boolean;
  botName: string;
  skills: BotSkillsState;
  onClose: () => void;
  onAddSkill: () => void;
  /** Open the skill library (browse, install, update, remove). */
  onBrowse: () => void;
}) {
  const { t } = useTranslation();
  const closeGuard = useRef<() => void>(onClose);
  return (
    <Modal open={open} onClose={() => closeGuard.current()} label={t("bots.skills.modalTitle")} size="md">
      {open ? (
        <SkillsDialog
          botName={botName}
          skills={skills}
          closeGuard={closeGuard}
          onClose={onClose}
          onAddSkill={onAddSkill}
          onBrowse={onBrowse}
        />
      ) : null}
    </Modal>
  );
}

function SkillsDialog({
  botName,
  skills,
  closeGuard,
  onClose,
  onAddSkill,
  onBrowse
}: {
  botName: string;
  skills: BotSkillsState;
  /** What the Modal calls on Esc / backdrop press. */
  closeGuard: MutableRefObject<() => void>;
  onClose: () => void;
  onAddSkill: () => void;
  onBrowse: () => void;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const { data } = skills;
  const [draft, setDraft] = useSkillsDraft(data);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmClose, setConfirmClose] = useState(false);

  const dirty = Boolean(data && draft && isBotSkillsDirty(draft, data));

  const pendingLeave = useRef<(() => void) | null>(null);
  const requestClose = () => {
    const action = closeAction(dirty, saving);
    if (action === "close") onClose();
    else if (action === "confirm") {
      pendingLeave.current = null;
      setConfirmClose(true);
    }
  };
  // Route changes (another bot, a chat, the palette) ask the same question as Cancel.
  useLeaveGuard({
    state: () => ({ dirty, busy: saving }),
    confirm: (proceed) => {
      pendingLeave.current = proceed;
      setConfirmClose(true);
    }
  });

  // Esc and the backdrop go through the same guard as Cancel and the X.
  useEffect(() => {
    closeGuard.current = requestClose;
  });

  const save = async () => {
    if (!data || !draft || saving) return;
    const payload = buildPutSkillsPayload(draft, data.available);
    if (exceedsSkillLimit(payload)) {
      setError(t("bots.skills.tooMany", { max: MAX_ASSIGNED_SKILLS }));
      return;
    }
    setSaving(true);
    setError(null);
    const result = await skills.save(payload);
    setSaving(false);
    if (result.ok) {
      toast.show(t("bots.skills.saved"), "success");
      onClose();
    } else if (result.failure === "conflict") {
      // Someone else saved first: their state was reloaded, never overwritten.
      setError(t("bots.skills.conflict"));
    } else {
      setError(
        result.failure === "invalid"
          ? t("bots.skills.invalid", { detail: result.message })
          : result.message
      );
    }
  };

  return (
    <div className={dialogStyles.root} data-testid="skills-dialog">
      <DialogHeader
        title={t("bots.skills.modalTitle")}
        subtitle={t("bots.skills.modalSubtitle", { name: botName })}
        closeDisabled={saving}
        onClose={requestClose}
        actions={
          <>
            <Button size="sm" variant="secondary" onClick={onBrowse} disabled={saving} data-testid="skills-browse-button">
              <Library aria-hidden="true" />
              {t("bots.skills.browseLibrary")}
            </Button>
            <Button size="sm" variant="secondary" onClick={onAddSkill} data-testid="skills-add-button">
              <Plus aria-hidden="true" />
              {t("addSkill.open")}
            </Button>
          </>
        }
      />
      <div className={dialogStyles.body} inert={saving}>
        {skills.error && !data ? (
          <Callout tone="danger" title={t("bots.skills.loadFailed")} role="alert">
            {skills.error}
          </Callout>
        ) : !data || !draft ? (
          <p className={dialogStyles.hint}>{t("bots.skills.loading")}</p>
        ) : (
          <SkillsEditor data={data} draft={draft} onChange={setDraft} />
        )}
        {error ? (
          <Callout tone="danger" role="alert">
            {error}
          </Callout>
        ) : null}
      </div>
      <DialogFooter
        note={
          dirty ? (
            <>
              <i className={dialogStyles.dirtyDot} aria-hidden="true" />
              {t("botEdit.unsaved")}
            </>
          ) : undefined
        }
      >
        {skills.error && !data ? (
          <Button variant="secondary" disabled={skills.loading} onClick={() => void skills.reload()}>
            {t("health.retry")}
          </Button>
        ) : null}
        <Button variant="ghost" disabled={saving} onClick={requestClose}>
          {t("common.cancel")}
        </Button>
        <Button
          variant="primary"
          disabled={saving || !dirty}
          data-testid="save-skills-button"
          onClick={() => void save()}
        >
          {t("common.save")}
        </Button>
      </DialogFooter>
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
    </div>
  );
}
