import { markApprovalDecided } from "../lib/approvalTried";
import { useCallback, useMemo, type ReactNode } from "react";
import { useDaemon } from "../state/DaemonContext";
import { isLocalBaseUrl } from "../lib/liveViewState";
import { ownerKey } from "../lib/ownerKey";
import { isAlreadyDecidedError } from "../lib/popout";
import { useTranslation } from "react-i18next";
import type { NeedsYouEntry } from "../state/timeline";
import { ApprovalCard, QuestionCard, type ApprovalCardProps } from "./ChatCards";
import { ElicitationCard } from "./ElicitationCard";
import { HandoverCard } from "./HandoverCard";
import { SecretRequestCard } from "./SecretRequestCard";
import { useOptionalLiveView } from "../state/LiveViewContext";
import { NoClientError, type SubmitBody } from "../lib/secretRequestForm";
import type { AnswerElicitation } from "../api/types";
import styles from "./PendingDecisions.module.css";

type Decide = ApprovalCardProps["onDecide"];

/**
 * What a card does when the user decides: the one place that talks to the daemon, so the chat's
 * inline cards, the pinned cards above the composer and the live view banner decide the same way.
 */
export function useDecisionActions() {
  const { client, recordAnswer, recordSecretOutcome, recordHandoverOutcome, recordElicitationOutcome, refreshRuns, home } =
    useDaemon();
  const liveView = useOptionalLiveView();
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
        try {
          await client.decideApproval(callId, decision, scope, reason, key);
          markApprovalDecided();
        } catch (err) {
          if (isAlreadyDecidedError(err)) {
            void refreshRuns();
          }
          throw err;
        }
      },
    [client, refreshRuns]
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
      try {
        await client.answerQuestion(callId, text);
        recordAnswer(runId, callId, text);
      } catch (err) {
        if (isAlreadyDecidedError(err)) {
          void refreshRuns();
        }
        throw err;
      }
    },
    [client, recordAnswer, refreshRuns]
  );
  // The value goes from the card to the daemon and nowhere else: only the outcome is recorded.
  const submitSecret = useCallback(
    (runId: string, callId: string) => async (body: SubmitBody) => {
      if (!client) throw new NoClientError();
      try {
        await client.submitSecretRequest(callId, body);
        recordSecretOutcome(runId, callId, "stored");
      } catch (err) {
        if (isAlreadyDecidedError(err)) {
          void refreshRuns();
        }
        throw err;
      }
    },
    [client, recordSecretOutcome, refreshRuns]
  );
  const cancelSecret = useCallback(
    (runId: string, callId: string) => async () => {
      if (!client) throw new NoClientError();
      try {
        await client.cancelSecretRequest(callId);
        recordSecretOutcome(runId, callId, "cancelled");
      } catch (err) {
        if (isAlreadyDecidedError(err)) {
          void refreshRuns();
        }
        throw err;
      }
    },
    [client, recordSecretOutcome, refreshRuns]
  );
  // A code goes from the card to the daemon and nowhere else: only the outcome is recorded.
  const submitCode = useCallback(
    (runId: string, callId: string) => async (code: string) => {
      if (!client) throw new NoClientError();
      await client.submitHandoverCode(callId, code);
      recordHandoverOutcome(runId, callId, "done");
    },
    [client, recordHandoverOutcome]
  );
  // The owner may still hold control of the browser: hand it back first, so the check the daemon
  // makes (and the bot's next look at the page) is not refused as "a person is driving".
  const handoverDone = useCallback(
    (runId: string, callId: string) => async () => {
      if (!client) throw new NoClientError();
      await liveView?.closeViewer();
      await client.finishHandover(callId);
      recordHandoverOutcome(runId, callId, "done");
    },
    [client, liveView, recordHandoverOutcome]
  );
  const cancelHandover = useCallback(
    (runId: string, callId: string) => async () => {
      if (!client) throw new NoClientError();
      await client.cancelHandover(callId);
      recordHandoverOutcome(runId, callId, "cancelled");
    },
    [client, recordHandoverOutcome]
  );
  // The answer goes from the card to the daemon (and on to the MCP server) only: the outcome is recorded.
  const answerElicitation = useCallback(
    (runId: string, callId: string) => async (body: AnswerElicitation) => {
      if (!client) throw new NoClientError();
      const res = await client.answerElicitation(callId, body);
      recordElicitationOutcome(runId, callId, res.outcome);
    },
    [client, recordElicitationOutcome]
  );
  const cancelElicitation = useCallback(
    (runId: string, callId: string) => async () => {
      if (!client) throw new NoClientError();
      const res = await client.cancelElicitation(callId);
      recordElicitationOutcome(runId, callId, res.outcome);
    },
    [client, recordElicitationOutcome]
  );
  const takeControl = useCallback(() => liveView?.openViewer(), [liveView]);
  return {
    decide,
    answer,
    submitSecret,
    cancelSecret,
    submitCode,
    handoverDone,
    cancelHandover,
    takeControl,
    answerElicitation,
    cancelElicitation,
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
        botId={entry.botId}
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
  if (entry.handover) {
    return (
      <HandoverCard
        item={entry.handover}
        botName={bots.find((b) => b.id === entry.botId)?.name ?? ""}
        onSubmitCode={actions.submitCode(entry.runId, entry.callId)}
        onDone={actions.handoverDone(entry.runId, entry.callId)}
        onCancel={actions.cancelHandover(entry.runId, entry.callId)}
        onTakeControl={actions.takeControl}
      />
    );
  }
  if (entry.elicitation) {
    return (
      <ElicitationCard
        item={entry.elicitation}
        onAnswer={actions.answerElicitation(entry.runId, entry.callId)}
        onCancel={actions.cancelElicitation(entry.runId, entry.callId)}
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
