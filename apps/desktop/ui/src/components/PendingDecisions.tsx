import { useCallback, useMemo, type ReactNode } from "react";
import { useDaemon } from "../state/DaemonContext";
import { isLocalBaseUrl } from "../lib/liveViewState";
import { ownerKey } from "../lib/ownerKey";
import { useTranslation } from "react-i18next";
import type { NeedsYouEntry } from "../state/timeline";
import { ApprovalCard, QuestionCard, type ApprovalCardProps } from "./ChatCards";
import { SecretRequestCard } from "./SecretRequestCard";
import { NoClientError, type SubmitBody } from "../lib/secretRequestForm";
import styles from "./PendingDecisions.module.css";

type Decide = ApprovalCardProps["onDecide"];

/**
 * What a card does when the user decides: the one place that talks to the daemon, so the chat's
 * inline cards, the pinned cards above the composer and the live view banner decide the same way.
 */
export function useDecisionActions() {
  const { client, recordAnswer, recordSecretOutcome, home } = useDaemon();
  const { t } = useTranslation();
  const decide = useCallback(
    (callId: string): Decide =>
      async (decision, scope, reason) => {
        if (!client) {
          return;
        }
        // A standing choice ("always for this chat") carries the owner's authority, which
        // only the desktop app's own window holds; a plain approval needs no key.
        const standing =
          decision === "approve" &&
          scope !== undefined &&
          scope !== "once" &&
          scope !== "run_control";
        const key = standing ? await ownerKey(client.baseUrl) : undefined;
        await client.decideApproval(callId, decision, scope, reason, key);
      },
    [client]
  );
  const trustFolder = useMemo(
    () =>
      client && isLocalBaseUrl(client.baseUrl)
        ? async (folder: string) => {
            const key = await ownerKey(client.baseUrl);
            if (!key) throw new Error(t("folderTrust.ownerRequired"));
            await client.setFolderTrust(folder, true, key);
          }
        : undefined,
    [client, t]
  );
  const answer = useCallback(
    (runId: string, callId: string) => async (text: string) => {
      if (!client) {
        return;
      }
      await client.answerQuestion(callId, text);
      recordAnswer(runId, callId, text);
    },
    [client, recordAnswer]
  );
  // The value goes from the card to the daemon and nowhere else: only the outcome is recorded.
  const submitSecret = useCallback(
    (runId: string, callId: string) => async (body: SubmitBody) => {
      if (!client) throw new NoClientError();
      await client.submitSecretRequest(callId, body);
      recordSecretOutcome(runId, callId, "stored");
    },
    [client, recordSecretOutcome]
  );
  const cancelSecret = useCallback(
    (runId: string, callId: string) => async () => {
      if (!client) throw new NoClientError();
      await client.cancelSecretRequest(callId);
      recordSecretOutcome(runId, callId, "cancelled");
    },
    [client, recordSecretOutcome]
  );
  return {
    decide,
    answer,
    submitSecret,
    cancelSecret,
    trustFolder,
    serverName: home.kind === "host" ? home.name : null
  };
}

/** One pending entry as its card (an approval or a question). */
export function PendingCard({ entry }: { entry: NeedsYouEntry }): ReactNode {
  const actions = useDecisionActions();
  const { bots } = useDaemon();
  if (entry.tool) {
    return (
      <ApprovalCard
        tool={entry.tool}
        serverName={actions.serverName}
        onTrustFolder={actions.trustFolder}
        onDecide={actions.decide(entry.callId)}
      />
    );
  }
  if (entry.question) {
    return (
      <QuestionCard
        q={entry.question}
        onAnswer={actions.answer(entry.runId, entry.callId)}
      />
    );
  }
  if (entry.secret) {
    return (
      <SecretRequestCard
        item={entry.secret}
        botName={bots.find((b) => b.id === entry.botId)?.name ?? ""}
        onSubmit={actions.submitSecret(entry.runId, entry.callId)}
        onCancel={actions.cancelSecret(entry.runId, entry.callId)}
      />
    );
  }
  return null;
}

/**
 * Cards that wait for the user, each with the Approve / Reject (or the answer box) right in it.
 * One entry, one card: the count of the list is the count of cards.
 */
export function PendingDecisions({
  entries,
  caption
}: {
  entries: readonly NeedsYouEntry[];
  /** A line above a card that belongs elsewhere (another chat of the bot). */
  caption?: (entry: NeedsYouEntry) => string | null;
}) {
  if (entries.length === 0) {
    return null;
  }
  return (
    <div className={styles.list} data-testid="pending-decisions">
      {entries.map((entry) => {
        const line = caption?.(entry) ?? null;
        return (
          <div
            key={entry.callId}
            className={styles.item}
            data-testid="pending-card"
            data-call-id={entry.callId}
          >
            {line ? <p className={styles.caption}>{line}</p> : null}
            <PendingCard entry={entry} />
          </div>
        );
      })}
    </div>
  );
}
