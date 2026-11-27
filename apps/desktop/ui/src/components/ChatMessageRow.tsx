import { Pencil } from "lucide-react";
import { memo, useState, type CSSProperties, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { BotAvatar } from "ui";
import type { ApiClient } from "../api/client";
import type { Bot, Message, Run } from "../api/types";
import { botView } from "../lib/botProfile";
import { splitMentions } from "../lib/mentionText";
import type { MessageTimes } from "../lib/messageTime";
import type { RunState } from "../state/timeline";
import { MessageFiles } from "./Attachments";
import { CopyButton } from "./CopyButton";
import { DocCitationProvider, useCitationOpener } from "./DocCitationLink";
import { Markdown } from "./Markdown";
import { MessageActions } from "./MessageActions";
import { MessageInfo } from "./MessageInfo";
import { MessageTime } from "./MessageTime";
import { EditMessage } from "./ResendTurn";
import { SpeakButton } from "./SpeakButton";
import { FoldedBody, UserBubble } from "./UserBubble";
import styles from "../views/ChatView.module.css";
import tipStyles from "./ActionTip.module.css";
import msgStyles from "./MessageActions.module.css";

/** Primitive props and stable message ids let completed rows sit out token and draft updates. */
export const UserMessageRow = memo(function UserMessageRow({
  id, msgId, ts, text, source, attachments, client, chatId, mentionBots, times,
  editable, editing, busy, usedWriteTools, onEdit, onSave, onCancel
}: {
  id: string; msgId: string; ts: number; text: string; source?: string | null;
  attachments: Message["attachments"]; client: ApiClient | null; chatId: string;
  mentionBots: readonly { name: string; color: string }[]; times: MessageTimes;
  editable: boolean; editing: boolean; busy: boolean; usedWriteTools: boolean;
  onEdit: (id: string) => void; onSave?: (text: string) => void; onCancel?: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div id={id} tabIndex={-1} className={styles.userRow} data-message-row="user">
      {editing ? (
        <EditMessage initial={text} usedWriteTools={usedWriteTools} busy={busy}
          onSave={(value) => onSave?.(value)} onCancel={() => onCancel?.()} />
      ) : (
        <UserBubble text={text}>
          <MessageFiles client={client} chatId={chatId} attachments={attachments ?? []} />
          {splitMentions(text, mentionBots).map((part, i) => part.color ? (
            <span key={i} className={styles.mention} style={{ "--mention-color": part.color } as CSSProperties}>{part.text}</span>
          ) : part.text)}
        </UserBubble>
      )}
      <MessageActions side="me" meta={<MessageTime ts={ts} mode={times} />}>
        <CopyButton text={text} />
        {editable && !editing ? (
          <button type="button" className={msgStyles.copyBtn} aria-label={t("chat.resend.edit")}
            data-testid="edit-message-button" onClick={() => onEdit(msgId)}>
            <Pencil aria-hidden="true" />
            <span className={tipStyles.tip} aria-hidden="true">{t("chat.resend.edit")}</span>
          </button>
        ) : null}
        <MessageInfo client={client} side="me" ts={ts} source={source} />
      </MessageActions>
    </div>
  );
});

export const AssistantMessageRow = memo(function AssistantMessageRow({
  id, ts, text, source, botId, runId, run, runDto, guest, times, client, runActions
}: {
  id: string; ts: number; text: string; source?: string | null; botId?: string | null;
  runId?: string | null; run?: RunState; runDto?: Run; guest?: Bot; times: MessageTimes;
  client: ApiClient | null;
  /** The finished run's actions, when this is its newest answer (the one row that re-renders with them). */
  runActions?: ReactNode;
}) {
  const { t } = useTranslation();
  const gv = guest ? botView(guest) : null;
  const [localModel, setLocalModel] = useState(false);
  const openCitation = useCitationOpener(client, guest?.id ?? botId);
  return (
    <div id={id} tabIndex={-1} className={styles.assistantRow} data-message-row="assistant">
      <div className={styles.assistantBubble} data-message-body>
        {source?.startsWith("imported") ? <span className={styles.importedBadge}>{t("chat.imported")}</span> : null}
        {localModel ? <span className={styles.importedBadge} data-testid="local-model-chip">{t("offline.localAnswer")}</span> : null}
        {gv ? (
          <span className={styles.authorName} style={{ "--mention-color": gv.color } as CSSProperties} data-testid="guest-author">
            <BotAvatar bot={gv} size="xs" decorative />
            <span className={styles.authorLabel}>{gv.name}</span>
          </span>
        ) : null}
        <FoldedBody text={text} className={styles.foldBody} answer>
          <DocCitationProvider value={openCitation}>
            <Markdown text={text} />
          </DocCitationProvider>
        </FoldedBody>
      </div>
      <MessageActions side="bot" runActions={runActions} meta={<MessageTime ts={ts} mode={times} />}>
        <CopyButton text={text} />
        <SpeakButton id={id} text={text} botId={botId} />
        <MessageInfo client={client} side="bot" ts={ts} source={source} runId={runId} run={run} runDto={runDto} onLocalModel={setLocalModel} />
      </MessageActions>
    </div>
  );
});
