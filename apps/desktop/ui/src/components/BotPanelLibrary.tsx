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
/** How many rows a shelf shows before "and N more" (the rest is one click away in Settings). */
const SHOWN = 3;

/** One shelf: icon, title, count and one action on the first line; what it is on the second. */
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
    <section className={styles.shelf} aria-labelledby={id} data-testid={testId}>
      <div className={styles.head}>
        <Icon className={styles.headIcon} aria-hidden="true" />
        <h3 className={styles.title} id={id}>
          {title}
        </h3>
        {count ? <span className={styles.count}>{count}</span> : null}
        {action ? <span className={styles.action}>{action}</span> : null}
      </div>
      <p className={styles.line}>{line}</p>
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
  const more = (n: number) => (n > SHOWN ? <li className={styles.more}>{t("library.more", { count: n - SHOWN })}</li> : null);

  return (
    <div className={styles.root} data-testid="bot-library">
      <p className={styles.intro}>{t("library.intro", { name: botName })}</p>
      {error ? (
        <p className={styles.error} role="alert">
          {error}
        </p>
      ) : null}

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
        <BotPanelSkills skills={skills} onManage={onManageSkills} onAdd={onAddSkill} bare />
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
          <ul className={styles.rows}>
            {shelves.documents.slice(0, SHOWN).map((s) => {
              const Icon = KIND_ICONS[s.kind] ?? FileText;
              return (
                <li key={s.id}>
                  <button type="button" className={styles.row} onClick={() => openCard("knowledge")}>
                    <Icon aria-hidden="true" />
                    <span className={styles.rowName}>{s.label || s.locator}</span>
                    <span className={styles.rowMeta}>
                      {s.kind === "folder"
                        ? t("library.documents.files", { count: s.documents })
                        : t(`library.kind.${s.kind}`)}
                      {s.enabled ? "" : ` · ${t("library.off")}`}
                    </span>
                  </button>
                </li>
              );
            })}
            {more(shelves.documents.length)}
          </ul>
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
          <ul className={styles.rows}>
            {shelves.notes.slice(0, SHOWN).map((s) => (
              <li key={s.id}>
                <button type="button" className={styles.row} onClick={() => openCard("knowledge")}>
                  <StickyNote aria-hidden="true" />
                  <span className={styles.rowName}>{s.label || t("library.notes.untitled")}</span>
                </button>
              </li>
            ))}
            {more(shelves.notes.length)}
          </ul>
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
