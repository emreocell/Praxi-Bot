import {
  Bell,
  BellOff,
  ChevronRight,
  Lock,
  PenLine,
  Plus,
  ShieldCheck,
  SlidersHorizontal,
  Star,
  UserRound,
  X,
  Zap
} from "lucide-react";
import { type CSSProperties, Fragment, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar, type BotAvatarState, type BotIdentity } from "ui";
import type { UpdateChatInput } from "../api/types";
import { activitySections, librarySections, subagentRows } from "../lib/activityPanel";
import { isSlowStep } from "../lib/stepStuck";
import { type PermissionTone, permissionTone } from "../lib/botEditor";
import { chatActivity, chatDateLabel } from "../lib/botLanding";
import { CHAT_SEARCH_MIN_CHATS, filterChats } from "../lib/chatSearch";
import type { PresetChoice } from "../lib/botProfile";
import { modelLine } from "../lib/modelName";
import { approvalHeadline, type T } from "../lib/approvalPlain";
import { runNow } from "../lib/runNow";
import { toolSentence } from "../lib/runSentence";
import { cancelRun } from "../state/appStop";
import { useDaemon } from "../state/DaemonContext";
import { useQuotas } from "../state/useQuotas";
import type { BotSkillsState } from "../state/useBotSkills";
import { BotComputerBlock } from "./BotComputerBlock";
import { StopStatusNotice } from "./StopStatusNotice";
import styles from "./BotPanel.module.css";
import { BotPanelMail } from "./BotPanelMail";
import { BotPanelWebhooks } from "./BotPanelWebhooks";
import { BotPanelSkills } from "./BotPanelSkills";
import { BrandMark } from "./BrandMark";
import { ChatHistorySearch } from "./ChatHistorySearch";
import { SEARCH_ALL_CHATS_EVENT, type SearchRequest, takeSearchRequest } from "../lib/chatSearchRequest";
import { ChatList } from "./ChatList";
import { QuotaBlock } from "./QuotaRings";
import { SubagentDetail } from "./SubagentDetail";
import { SubagentList, useNow } from "./SubagentList";
import { formatElapsed } from "../lib/activityPanel";
import { UpcomingBlock, useUpcoming } from "./UpcomingBlock";

export interface BotPanelChat {
  id: string;
  title: string;
  createdAtMs: number;
  pinned?: boolean;
  archived?: boolean;
}

const TONE_ICONS: Record<PermissionTone, typeof Lock> = {
  locked: Lock,
  guarded: ShieldCheck,
  open: Zap,
  custom: SlidersHorizontal
};

/** The permission mode as a small tinted chip (shared with the profile dialog). */
export function PermissionChip({ preset }: { preset: PresetChoice }) {
  const { t } = useTranslation();
  const tone = permissionTone(preset);
  const Icon = TONE_ICONS[tone];
  // "Permission level: Full access. <what it allows>": the chip's one word alone says nothing.
  const label = `${t("botPanel.permissionLabel", { level: t(`botForm.preset.${preset}.name`) })}. ${t(`botForm.preset.${preset}.hint`)}`;
  return (
    <span
      className={styles.chip}
      data-tone={tone}
      title={label}
      role="img"
      aria-label={label}
      data-testid="bot-permission-chip"
    >
      <Icon aria-hidden="true" />
      <span>{t(`botForm.preset.${preset}.name`)}</span>
    </span>
  );
}

/** The model a bot runs on, by its human name (never the raw id). */
export function ModelLineText({ model }: { model: string | undefined }) {
  const { t } = useTranslation();
  const line = modelLine(model);
  if (!line.name) {
    return <span className={styles.modelMuted}>{t("modelPicker.connectModel")}</span>;
  }
  return (
    <span className={styles.modelLine} title={model} data-testid="bot-model-line">
      {line.brand ? <BrandMark name={line.brand} size={14} /> : null}
      <span className={styles.modelName}>{line.name}</span>
    </span>
  );
}

/**
 * Right panel of the app shell: everything about the selected bot in one place (owner feedback
 * 244, point 4) - who it is, the quick actions, its model and permission mode, its skills, its
 * chats and the computer it works on.  The center only ever shows a chat.
 */
export function BotPanel({
  bot,
  model,
  preset,
  engineCli,
  state,
  chats,
  activeChatId,
  needs,
  runs,
  pendingChatIds,
  skills,
  newChatBusy,
  visible = true,
  onClose,
  onOpenChat,
  onNewChat,
  onPatchChat,
  onDeleteChat,
  onOpenProfile,
  onOpenSettings,
  onManageSkills,
  onAddSkill,
  onOpenMail,
  isMain = false,
  mainBotName,
  onMakeMain,
  notifyMuted = false,
  onToggleNotifyMute
}: {
  bot: BotIdentity;
  /** `provider/model` the bot runs on. */
  model?: string;
  preset: PresetChoice;
  /** Name of the CLI that runs this bot's files and shell (native-tools engine), if any. */
  engineCli?: string | null;
  state?: BotAvatarState;
  chats: readonly BotPanelChat[];
  activeChatId?: string;
  needs: ReadonlyArray<{ chatId: string }>;
  runs: ReadonlyArray<{ chat_id?: string | null; state: string }>;
  pendingChatIds: ReadonlySet<string>;
  skills: BotSkillsState;
  newChatBusy: boolean;
  /** The panel is on screen (closing it ends the computer view). */
  visible?: boolean;
  onClose: () => void;
  onOpenChat: (chatId: string, messageId?: string) => void;
  onNewChat: () => void;
  onPatchChat: (chatId: string, patch: UpdateChatInput) => Promise<void>;
  onDeleteChat: (chatId: string) => Promise<void>;
  onOpenProfile: () => void;
  onOpenSettings: () => void;
  onManageSkills: () => void;
  onAddSkill: () => void;
  /** Open Settings, Mail (connect, edit or remove a mailbox). */
  onOpenMail: () => void;
  /** This bot is the main bot (first in the list). */
  isMain?: boolean;
  /** Name of the main bot (the computer card says who is main). */
  mainBotName?: string;
  /** Make this bot the main bot; omitted when there is nothing to choose. */
  onMakeMain?: () => void;
  /** This bot's long-run / failed notifications are muted (approvals still notify). */
  notifyMuted?: boolean;
  /** Mute or unmute those notifications; omitted hides the switch. */
  onToggleNotifyMute?: () => void;
}) {
  const { t, i18n } = useTranslation();
  const { events, refreshRuns } = useDaemon();
  const [tab, setTab] = useState<"activity" | "automation" | "library">("activity");
  const [openSub, setOpenSub] = useState<string | null>(null);
  // "Search all chats" from a right-click menu lands on the chats list with the phrase filled in.
  const [historySeed, setHistorySeed] = useState<SearchRequest | null>(null);
  useEffect(() => {
    const take = () => {
      const request = takeSearchRequest();
      if (request) {
        setTab("library");
        setHistorySeed(request);
      }
    };
    take();
    window.addEventListener(SEARCH_ALL_CHATS_EVENT, take);
    return () => window.removeEventListener(SEARCH_ALL_CHATS_EVENT, take);
  }, []);
  const [chatQuery, setChatQuery] = useState("");
  const visibleChats = filterChats(chats, chatQuery, t("botPanel.untitled"));
  const chatItems = visibleChats.map((c) => ({
    ...c,
    dateLabel: chatDateLabel(c.createdAtMs, i18n.language),
    activity: chatActivity(c.id, needs, runs)
  }));
  const subs = subagentRows(events, activeChatId);
  const upcoming = useUpcoming(bot.id);
  const quotas = useQuotas();
  const activity = activitySections({ subagents: subs.length, upcoming: upcoming.rows.length, quota: quotas.length });
  const now = runNow(events, activeChatId);
  const nowTick = useNow(now !== null);
  const library = librarySections({ chats: chats.length });
  const stopSub = (id: string) => {
    cancelRun(id);
    void refreshRuns();
  };
  return (
    <div className={styles.root} id="bot-panel" data-testid="bot-panel">
      <button
        type="button"
        className={styles.close}
        title={t("botPanel.close")}
        aria-label={t("botPanel.close")}
        onClick={onClose}
      >
        <X aria-hidden="true" />
      </button>

      <header className={styles.hero}>
        <div className={styles.ava} style={{ "--bot": bot.color } as CSSProperties}>
          <BotAvatar bot={bot} size="panel" state={state ?? "idle"} decorative />
        </div>
        <div className={styles.nameRow}>
          <h2 className={styles.name}>{bot.name}</h2>
          {onMakeMain ? (
            <button
              type="button"
              className={styles.mainBtn}
              data-testid="bot-panel-make-main"
              title={isMain ? t("botPanel.isMain") : t("botPanel.makeMain")}
              aria-label={isMain ? t("botPanel.isMain") : t("botPanel.makeMain")}
              aria-pressed={isMain}
              aria-disabled={isMain}
              onClick={isMain ? undefined : onMakeMain}
            >
              <Star aria-hidden="true" fill={isMain ? "currentColor" : "none"} />
            </button>
          ) : null}
        </div>
        <div className={styles.meta}>
          <ModelLineText model={model} />
          <PermissionChip preset={preset} />
          {engineCli ? <span data-testid="bot-engine-label">{t("botEngine.label", { cli: engineCli })}</span> : null}
        </div>
        <div className={styles.actions}>
          <button
            type="button"
            className={styles.primary}
            disabled={newChatBusy}
            data-testid="bot-panel-new-chat"
            onClick={onNewChat}
          >
            <Plus aria-hidden="true" />
            {t("bots.newChat")}
          </button>
          <button
            type="button"
            className={styles.iconBtn}
            data-testid="bot-panel-edit"
            title={t("botPanel.edit")}
            aria-label={t("botPanel.edit")}
            onClick={onOpenSettings}
          >
            <PenLine aria-hidden="true" />
          </button>
          <button
            type="button"
            className={styles.iconBtn}
            data-testid="bot-panel-profile"
            title={t("botPanel.profile")}
            aria-label={t("botPanel.profile")}
            onClick={onOpenProfile}
          >
            <UserRound aria-hidden="true" />
          </button>
          {onToggleNotifyMute ? (
            // A toggle: the name stays the same, aria-pressed says whether the mute is on.
            <button
              type="button"
              className={styles.iconBtn}
              data-testid="bot-panel-mute"
              title={`${t("botPanel.muteNotify")}. ${t("botPanel.muteNotifyHint")}`}
              aria-label={t("botPanel.muteNotify")}
              aria-pressed={notifyMuted}
              onClick={onToggleNotifyMute}
            >
              {notifyMuted ? <BellOff aria-hidden="true" /> : <Bell aria-hidden="true" />}
            </button>
          ) : null}
        </div>
      </header>

      <div className={styles.tabs} role="tablist" aria-label={t("botPanel.tabs.label")}>
        {(["activity", "automation", "library"] as const).map((id, index, tabs) => (
          <button
            key={id}
            type="button"
            role="tab"
            id={`bot-panel-tab-${id}`}
            aria-selected={tab === id}
            aria-controls="bot-panel-tabpanel"
            tabIndex={tab === id ? 0 : -1}
            className={styles.tab}
            data-testid={`bot-panel-tab-${id}`}
            onClick={() => setTab(id)}
            onKeyDown={(e) => {
              if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
              e.preventDefault();
              const step = e.key === "ArrowRight" ? 1 : -1;
              const next = tabs[(index + step + tabs.length) % tabs.length];
              setTab(next);
              document.getElementById(`bot-panel-tab-${next}`)?.focus();
            }}
          >
            {t(`botPanel.tabs.${id}`)}
          </button>
        ))}
      </div>

      <div className={styles.tabPanel} role="tabpanel" id="bot-panel-tabpanel" aria-labelledby={`bot-panel-tab-${tab}`}>
        {tab === "activity" ? (
          <>
            <StopStatusNotice variant="panel" />
            {now ? (
              <div className={styles.now} data-kind={now.approval ? "approval" : "run"} data-testid="bot-panel-now">
                <span className={styles.pulse} aria-hidden="true" />
                <div className={styles.nowText}>
                  <b>
                    {now.approval
                      ? approvalHeadline(
                          now.approval.name,
                          now.approval.input,
                          now.approval.details,
                          { computer: null, serverName: null },
                          t as unknown as T
                        )
                      : now.step
                        ? toolSentence(t, now.step)
                        : t("botPanel.now.thinking")}
                  </b>
                  <small>
                    {[
                      now.subagents > 0 ? t("botPanel.now.subagents", { count: now.subagents }) : null,
                      !now.approval && now.step && isSlowStep(now.step, nowTick) ? t("chat.chain.slow") : null,
                      formatElapsed(nowTick - now.startedMs)
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </small>
                </div>
                <button
                  type="button"
                  className={styles.nowOpen}
                  title={t("botPanel.now.open")}
                  aria-label={t("botPanel.now.open")}
                  data-testid="bot-panel-now-open"
                  onClick={() => setOpenSub(now.run.id)}
                >
                  <ChevronRight aria-hidden="true" />
                </button>
              </div>
            ) : null}
            {activity.map((id) => {
              switch (id) {
                case "subagents":
                  return <SubagentList key={id} rows={subs} onOpen={setOpenSub} onStop={stopSub} />;
                case "upcoming":
                  return <UpcomingBlock key={id} rows={upcoming.rows} onToggle={upcoming.toggle} />;
                case "computer":
                  return <BotComputerBlock
                      key={id}
                      botName={bot.name}
                      botId={bot.id}
                      mainBotName={mainBotName}
                      activity={now ? (now.approval ? t("botPanel.now.waiting") : now.step ? toolSentence(t, now.step) : t("botPanel.now.thinking")) : null}
                      visible={visible}
                    />;
                default:
                  return <QuotaBlock key={id} />;
              }
            })}
          </>
        ) : tab === "automation" ? (
          <BotPanelWebhooks botId={bot.id} />
        ) : (
          library.map((id) =>
            id === "skills" ? (
              <Fragment key={id}>
                <BotPanelSkills skills={skills} onManage={onManageSkills} onAdd={onAddSkill} />
                <BotPanelMail botId={bot.id} botName={bot.name} onOpenMail={onOpenMail} />
              </Fragment>
            ) : (
              <div key={id}>
        <section className={styles.section} aria-labelledby="bot-panel-chats">
        <h3 className={styles.label} id="bot-panel-chats">
          {t("botPanel.chats")}
        </h3>
        <ChatHistorySearch onOpenChat={onOpenChat} seed={historySeed} />
        {chats.length >= CHAT_SEARCH_MIN_CHATS ? (
          <input
            type="search"
            className={styles.chatSearch}
            value={chatQuery}
            placeholder={t("botPanel.searchChats")}
            aria-label={t("botPanel.searchChats")}
            onChange={(e) => setChatQuery(e.target.value)}
          />
        ) : null}
        {visibleChats.length === 0 && chatQuery.trim() ? (
          <p className={styles.chatSearchEmpty} role="status">{t("botPanel.noChatMatches")}</p>
        ) : (
          <ChatList
            chats={chatItems}
            activeChatId={activeChatId}
            runs={runs}
            pendingIds={pendingChatIds}
            variant="panel"
            searchActive={chatQuery.trim().length > 0}
            onOpenChat={onOpenChat}
            onPatchChat={onPatchChat}
            onDeleteChat={onDeleteChat}
          />
        )}
      </section>
              </div>
            )
          )
        )}
      </div>
      <SubagentDetail
        run={openSub ? events.runs[openSub] : undefined}
        fallbackTitle={t("botPanel.now.detailTitle")}
        onClose={() => setOpenSub(null)}
        onStop={stopSub}
      />
    </div>
  );
}
