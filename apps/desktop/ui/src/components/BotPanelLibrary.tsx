import {
  BookOpen,
  Brain,
  ChevronRight,
  FileText,
  Folder,
  FolderOpen,
  Globe,
  type LucideIcon,
  Mail,
  MessagesSquare,
  Plus,
  Sparkles,
  StickyNote
} from "lucide-react";
import { type ReactNode, useCallback, useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import type { KnowledgeSource, KnowledgeSourceKind, MemoryItem } from "../api/types";
import { humanLine } from "../lib/humanError";
import { libraryShelves, libraryTarget } from "../lib/library";
import { openSettingsTarget } from "../lib/settingsLink";
import { useDaemon } from "../state/DaemonContext";
import type { BotSkillsState } from "../state/useBotSkills";
import { BotPanelSkills } from "./BotPanelSkills";
import { PanelGroup, PanelRows } from "./PanelRows";
import { BotPanelWorkspace } from "./BotPanelWorkspace";
import { useToast } from "./Toast";
import styles from "./BotPanelLibrary.module.css";

const KIND_ICONS: Record<KnowledgeSourceKind, LucideIcon> = {
  file: FileText,
  folder: Folder,
  saved_web: Globe,
  conversation: MessagesSquare,
  email: Mail,
  note: StickyNote,
  shared_brain: Brain
};
/**
 * One shelf: icon, title, count and one action on one line, then its rows. What the shelf is
 * ("Files, folders and saved pages it looks things up in") is its description for assistive
 * technology; sighted users get it from the title and, while it is empty, from the empty text.
 */
function Shelf({
  id,
  icon: Icon,
  title,
  count,
  line,
  action,
  testId,
  children
}: {
  id: string;
  icon: LucideIcon;
  title: string;
  count?: string | null;
  line: string;
  action?: ReactNode;
  testId: string;
  children?: ReactNode;
}) {
  return (
    <section className={styles.shelf} aria-labelledby={id} aria-describedby={`${id}-line`} data-testid={testId}>
      <div className={styles.head}>
        <Icon className={styles.headIcon} aria-hidden="true" />
        <h3 className={styles.title} id={id}>
          {title}
        </h3>
        {count ? <span className={styles.count}>{count}</span> : null}
        {action ? <span className={styles.action}>{action}</span> : null}
      </div>
      <p className={styles.srOnly} id={`${id}-line`}>
        {line}
      </p>
      {children}
    </section>
  );
}

/**
 * The bot panel's Library tab: what this bot knows and keeps. Skills (how it does things),
 * documents (what it looks things up in), notes (what you told it), memory (what it picked up)
 * and its files. Each shelf says what it is in one line and has one way to add or open more;
 * adding documents and opening memory go to the same Settings cards, opened on this bot.
 */
export function BotPanelLibrary({
  botId,
  botName,
  active,
  skills,
  onManageSkills,
  onAddSkill,
  chatCount,
  chats
}: {
  botId: string;
  botName: string;
  active: boolean;
  skills: BotSkillsState;
  onManageSkills: () => void;
  onAddSkill: () => void;
  /** The bot's chats; the shelf is left out while there are none. */
  chatCount: number;
  chats: ReactNode;
}) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const [sources, setSources] = useState<KnowledgeSource[] | null>(null);
  const [memories, setMemories] = useState<MemoryItem[] | null>(null);
  const [pending, setPending] = useState<MemoryItem[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [noteOpen, setNoteOpen] = useState(false);

  const load = useCallback(async () => {
    if (!client) return;
    const [known, kept, asks] = await Promise.allSettled([
      client.listKnowledgeSources(botId),
      client.listMemories({ bot: botId }),
      client.listPendingMemories()
    ]);
    setSources(known.status === "fulfilled" ? known.value : []);
    setMemories(kept.status === "fulfilled" ? kept.value : []);
    setPending(asks.status === "fulfilled" ? asks.value : []);
    const failed = [known, kept].find((r) => r.status === "rejected") as PromiseRejectedResult | undefined;
    setError(failed ? humanLine(failed.reason) : null);
  }, [botId, client]);

  useEffect(() => {
    setSources(null);
    setMemories(null);
    setNoteOpen(false);
    if (active) void load();
  }, [active, load]);

  const shelves = libraryShelves(botId, sources ?? [], memories ?? [], pending);
  const loading = sources === null || memories === null;
  const openCard = (card: "knowledge" | "memory") => openSettingsTarget(libraryTarget(card, botId));

  return (
    <div className={styles.root} role="region" aria-label={t("library.intro", { name: botName })} data-testid="bot-library">
      {error ? (
        <p className={styles.error} role="alert">
          {error}
        </p>
      ) : null}

      <PanelGroup title={t("panelTabs.library.knows")} testId="library-group-knows">
      <Shelf
        id="library-skills"
        icon={Sparkles}
        title={t("library.skills.title")}
        line={t("library.skills.line")}
        testId="library-skills"
        action={
          <button type="button" className={styles.add} data-testid="bot-panel-add-skill" onClick={onAddSkill}>
            <Plus aria-hidden="true" />
            {t("library.add")}
          </button>
        }
      >
        <BotPanelSkills skills={skills} onManage={onManageSkills} />
      </Shelf>

      <Shelf
        id="library-documents"
        icon={BookOpen}
        title={t("library.documents.title")}
        count={shelves.documents.length ? t("library.documents.count", { count: shelves.documentCount }) : null}
        line={t("library.documents.line")}
        testId="library-documents"
        action={
          <button type="button" className={styles.add} data-testid="library-add-document" onClick={() => openCard("knowledge")}>
            <Plus aria-hidden="true" />
            {t("library.add")}
          </button>
        }
      >
        {loading ? null : shelves.documents.length === 0 ? (
          <p className={styles.empty}>{t("library.documents.empty")}</p>
        ) : (
          <PanelRows
            testId="library-document-rows"
            label={t("library.documents.title")}
            searchLabel={t("panelTabs.library.searchDocuments")}
            rows={shelves.documents.map((d) => ({
              id: d.id,
              name: d.label || d.locator,
              icon: KIND_ICONS[d.kind] ?? FileText,
              meta: d.kind === "folder" ? t("library.documents.files", { count: d.documents }) : t(`library.kind.${d.kind}`),
              status: d.enabled ? undefined : { tone: "muted", label: t("library.off") },
              onOpen: () => openCard("knowledge")
            }))}
          />
        )}
      </Shelf>

      <Shelf
        id="library-notes"
        icon={StickyNote}
        title={t("library.notes.title")}
        count={shelves.notes.length ? String(shelves.notes.length) : null}
        line={t("library.notes.line")}
        testId="library-notes"
        action={
          noteOpen ? null : (
            <button type="button" className={styles.add} data-testid="library-add-note" onClick={() => setNoteOpen(true)}>
              <Plus aria-hidden="true" />
              {t("library.add")}
            </button>
          )
        }
      >
        {noteOpen ? (
          <NoteForm
            botId={botId}
            onDone={async (saved) => {
              setNoteOpen(false);
              if (saved) await load();
            }}
          />
        ) : null}
        {loading || noteOpen ? null : shelves.notes.length === 0 ? (
          <p className={styles.empty}>{t("library.notes.empty")}</p>
        ) : (
          <PanelRows
            testId="library-note-rows"
            label={t("library.notes.title")}
            searchLabel={t("panelTabs.library.searchNotes")}
            rows={shelves.notes.map((n) => ({
              id: n.id,
              name: n.label || t("library.notes.untitled"),
              icon: StickyNote,
              onOpen: () => openCard("knowledge")
            }))}
          />
        )}
      </Shelf>

      <Shelf
        id="library-memory"
        icon={Brain}
        title={t("library.memory.title")}
        count={shelves.remembered ? String(shelves.remembered) : null}
        line={t("library.memory.line")}
        testId="library-memory"
        action={
          <button type="button" className={styles.add} data-testid="library-open-memory" onClick={() => openCard("memory")}>
            {t("library.open")}
            <ChevronRight aria-hidden="true" />
          </button>
        }
      >
        {loading ? null : shelves.waiting > 0 ? (
          <button type="button" className={styles.notice} data-testid="library-memory-waiting" onClick={() => openCard("memory")}>
            {t("library.memory.waiting", { count: shelves.waiting })}
            <ChevronRight aria-hidden="true" />
          </button>
        ) : shelves.remembered === 0 ? (
          <p className={styles.empty}>{t("library.memory.empty")}</p>
        ) : null}
      </Shelf>

      </PanelGroup>

      <PanelGroup title={t("panelTabs.library.keeps")} testId="library-group-keeps">
      <Shelf
        id="library-files"
        icon={FolderOpen}
        title={t("workspace.sectionTitle")}
        line={t("library.files.line")}
        testId="library-files"
      >
        <BotPanelWorkspace botId={botId} active={active} />
      </Shelf>

      {chatCount > 0 ? (
        <Shelf
          id="library-chats"
          icon={MessagesSquare}
          title={t("botPanel.chats")}
          count={String(chatCount)}
          line={t("library.chats.line")}
          testId="library-chats"
        >
          {chats}
        </Shelf>
      ) : null}
      </PanelGroup>
    </div>
  );
}

/** A note in two fields: what to keep in mind, and an optional title. */
function NoteForm({ botId, onDone }: { botId: string; onDone: (saved: boolean) => Promise<void> }) {
  const { t } = useTranslation();
  const { client } = useDaemon();
  const toast = useToast();
  const id = useId();
  const [title, setTitle] = useState("");
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const save = async () => {
    const content = text.trim();
    if (!client || !content || busy) return;
    setBusy(true);
    try {
      const label = title.trim() || content.split(/\s+/).slice(0, 6).join(" ");
      await client.createKnowledgeSource(botId, { kind: "note", label, content, html: false });
      toast.show(t("library.notes.saved"), "success");
      await onDone(true);
    } catch (e) {
      toast.show(humanLine(e), "danger");
    } finally {
      setBusy(false);
    }
  };
  return (
    <form
      className={styles.noteForm}
      data-testid="library-note-form"
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      <label className={styles.srOnly} htmlFor={`${id}-text`}>
        {t("library.notes.textLabel")}
      </label>
      <textarea
        id={`${id}-text`}
        className={styles.input}
        rows={3}
        maxLength={20000}
        value={text}
        autoFocus
        placeholder={t("library.notes.placeholder")}
        data-testid="library-note-text"
        onChange={(e) => setText(e.target.value)}
      />
      <label className={styles.srOnly} htmlFor={`${id}-title`}>
        {t("library.notes.titleLabel")}
      </label>
      <input
        id={`${id}-title`}
        className={styles.input}
        maxLength={120}
        value={title}
        placeholder={t("library.notes.titlePlaceholder")}
        onChange={(e) => setTitle(e.target.value)}
      />
      <div className={styles.formActions}>
        <button type="submit" className={styles.primary} disabled={busy || !text.trim()} data-testid="library-note-save">
          {t("library.notes.save")}
        </button>
        <button type="button" className={styles.ghost} disabled={busy} onClick={() => void onDone(false)}>
          {t("library.cancel")}
        </button>
      </div>
    </form>
  );
}
