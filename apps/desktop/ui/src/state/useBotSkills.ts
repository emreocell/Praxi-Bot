import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError, type ApiClient } from "../api/client";
import type { BotSkills, PutBotSkillsInput } from "../api/types";
import {
  type BotSkillsDraft,
  type SkillsSaveFailure,
  skillsSaveFailure,
  syncSkillsDraft
} from "../lib/botSkills";
import { humanLine } from "../lib/humanError";

export type SkillsSaveResult =
  | { ok: true; data: BotSkills }
  | { ok: false; failure: SkillsSaveFailure; message: string };

export interface BotSkillsState {
  botId: string | null;
  data: BotSkills | null;
  loading: boolean;
  error: string | null;
  /** Re-read the assignment and the catalog (the daemon re-scans the skill folders). */
  reload: () => Promise<BotSkills | null>;
  save: (payload: PutBotSkillsInput) => Promise<SkillsSaveResult>;
}

/**
 * One bot's skills (`GET|PUT /v1/bots/{id}/skills`), shared by the right panel, the skills
 * dialog and the edit dialog so all of them show the same assignment.
 */
export function useBotSkills(botId: string | null, client: ApiClient | null): BotSkillsState {
  const [data, setData] = useState<BotSkills | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Answers for a bot the user has already left must not land on the next one.
  const current = useRef(botId);
  current.current = botId;

  const reload = useCallback(async (): Promise<BotSkills | null> => {
    if (!client || !botId) {
      return null;
    }
    setLoading(true);
    setError(null);
    try {
      const next = await client.getBotSkills(botId);
      if (current.current === botId) {
        setData(next);
      }
      return next;
    } catch (e) {
      if (current.current === botId) {
        setError(humanLine(e));
      }
      return null;
    } finally {
      if (current.current === botId) {
        setLoading(false);
      }
    }
  }, [botId, client]);

  useEffect(() => {
    setData(null);
    setError(null);
    void reload();
  }, [reload]);

  const save = useCallback(
    async (payload: PutBotSkillsInput): Promise<SkillsSaveResult> => {
      if (!client || !botId) {
        return { ok: false, failure: "error", message: humanLine({ status: 503, code: "unavailable", message: "" }) };
      }
      try {
        const next = await client.putBotSkills(botId, payload);
        if (current.current === botId) {
          setData(next);
        }
        return { ok: true, data: next };
      } catch (e) {
        const failure = skillsSaveFailure(e instanceof ApiError ? e.status : null);
        if (failure === "conflict") {
          void reload();
        }
        return {
          ok: false,
          failure,
          message: humanLine(e)
        };
      }
    },
    [botId, client, reload]
  );

  return { botId, data, loading, error, reload, save };
}

/** The editable copy of a bot's skill assignment, kept in step with the server (`syncSkillsDraft`). */
export function useSkillsDraft(
  data: BotSkills | null
): [BotSkillsDraft | null, (next: BotSkillsDraft | null) => void] {
  const [draft, setDraft] = useState<BotSkillsDraft | null>(() =>
    data ? syncSkillsDraft(null, data) : null
  );
  const seen = useRef<BotSkills | null>(data);
  useEffect(() => {
    const previous = seen.current;
    seen.current = data;
    if (data) {
      setDraft((cur) => syncSkillsDraft(cur, data, previous));
    }
  }, [data]);
  return [draft, setDraft];
}
