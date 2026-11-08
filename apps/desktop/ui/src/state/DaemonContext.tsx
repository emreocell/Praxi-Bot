import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  useState
} from "react";
import { ApiClient } from "../api/client";
import { type Connection, connectDaemon } from "../api/connection";
import { subscribeEvents } from "../api/events";
import { MOCK_BASE_URL, MockApiClient } from "../api/mock";
import type { Bot, Chat, GlobalEnvelope, Health, Run } from "../api/types";
import {
  applyEvent,
  EMPTY_EVENTS,
  type EventsState,
  markAnswered,
  type NeedsYouEntry,
  needsYou,
  reconcile
} from "./timeline";

export type Phase = "connecting" | "ready" | "error";

/** Runs in these states can still produce approvals / questions. */
const LIVE_RUN_STATES = new Set(["pending", "running", "waiting_approval"]);

type Action =
  | { type: "event"; env: GlobalEnvelope }
  | { type: "answered"; runId: string; callId: string; answer: string }
  | {
      type: "reconcile";
      approvals: ReadonlySet<string>;
      live: ReadonlySet<string>;
      asOfMs: number;
    };

function reducer(state: EventsState, action: Action): EventsState {
  switch (action.type) {
    case "event":
      return applyEvent(state, action.env);
    case "answered":
      return markAnswered(state, action.runId, action.callId, action.answer);
    case "reconcile":
      return reconcile(state, action.approvals, action.live, action.asOfMs);
  }
}

export interface DaemonContextValue {
  phase: Phase;
  error: string | null;
  conn: Connection | null;
  client: ApiClient | null;
  health: Health | null;
  eventsConnected: boolean;
  bots: Bot[];
  chats: Chat[];
  runs: Run[];
  events: EventsState;
  needs: NeedsYouEntry[];
  refreshBots: () => Promise<void>;
  refreshChats: () => Promise<void>;
  refreshRuns: () => Promise<void>;
  recordAnswer: (runId: string, callId: string, answer: string) => void;
  retry: () => void;
  restartDaemon: () => Promise<void>;
  /** Listen to the raw daemon events (live and replayed); returns the unsubscribe. */
  onDaemonEvent: (listener: (env: GlobalEnvelope) => void) => () => void;
}

const Ctx = createContext<DaemonContextValue | null>(null);

export function useDaemon(): DaemonContextValue {
  const v = useContext(Ctx);
  if (!v) {
    throw new Error("useDaemon outside DaemonProvider");
  }
  return v;
}

export function DaemonProvider({ children }: { children: ReactNode }) {
  const [phase, setPhase] = useState<Phase>("connecting");
  const [error, setError] = useState<string | null>(null);
  const [conn, setConn] = useState<Connection | null>(null);
  const [health, setHealth] = useState<Health | null>(null);
  const [eventsConnected, setEventsConnected] = useState(false);
  const [bots, setBots] = useState<Bot[]>([]);
  const [chats, setChats] = useState<Chat[]>([]);
  const [runs, setRuns] = useState<Run[]>([]);
  const [events, dispatch] = useReducer(reducer, EMPTY_EVENTS);
  const [attempt, setAttempt] = useState(0);

  const client = useMemo(
    () =>
      conn
        ? conn.base_url === MOCK_BASE_URL
          ? new MockApiClient()
          : new ApiClient(conn)
        : null,
    [conn]
  );
  const clientRef = useRef<ApiClient | null>(null);
  clientRef.current = client;
  const listeners = useRef(new Set<(env: GlobalEnvelope) => void>());
  const onDaemonEvent = useCallback(
    (listener: (env: GlobalEnvelope) => void) => {
      listeners.current.add(listener);
      return () => {
        listeners.current.delete(listener);
      };
    },
    []
  );

  // 1. find / start the daemon
  useEffect(() => {
    let cancelled = false;
    setPhase("connecting");
    connectDaemon()
      .then((c) => {
        if (!cancelled) {
          setConn(c);
          setPhase("ready");
          setError(null);
        }
      })
      .catch((e: unknown) => {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : String(e));
          setPhase("error");
        }
      });
    return () => {
      cancelled = true;
    };
  }, [attempt]);

  const refreshBots = useCallback(async () => {
    const c = clientRef.current;
    if (c) {
      setBots(await c.listBots());
    }
  }, []);
  const refreshChats = useCallback(async () => {
    const c = clientRef.current;
    if (c) {
      setChats(await c.listChats());
    }
  }, []);
  const refreshRuns = useCallback(async () => {
    const c = clientRef.current;
    if (c) {
      setRuns(await c.listRuns());
    }
  }, []);

  // 2. initial data + event stream (replayed from the start: `Last-Event-ID: 0`)
  useEffect(() => {
    if (!client) {
      return undefined;
    }
    void Promise.all([refreshBots(), refreshChats(), refreshRuns()]).catch(
      () => {}
    );
    if (client instanceof MockApiClient) {
      setEventsConnected(true);
      if (new URLSearchParams(window.location.search).get("screen") !== "empty") {
        const runId = "mock-review-run";
        const events: GlobalEnvelope["event"][] = [
          { type: "run_started", run_id: runId, bot_id: "11111111-1111-4111-8111-111111111111", chat_id: "33333333-3333-4333-8333-333333333333" },
          { type: "tool_call_requested", run_id: runId, call_id: "mock-tool", tool_name: "github.search", input: { query: "is:closed updated:this-week" } },
          { type: "tool_call_completed", run_id: runId, call_id: "mock-tool", error: null },
          { type: "run_finished", run_id: runId, error: null }
        ];
        events.forEach((event, i) => dispatch({ type: "event", env: { v: 1, seq: i + 1, gseq: i + 1, ts_ms: 1750000000000 + (i === 3 ? 2000 : 200 + i * 100), event } }));
      }
      return undefined;
    }
    return subscribeEvents(client, {
      lastEventId: 0,
      onEvent: (env) => {
        dispatch({ type: "event", env });
        for (const l of listeners.current) {
          l(env);
        }
      },
      onConnection: setEventsConnected
    });
  }, [client, refreshBots, refreshChats, refreshRuns]);

  // 3. health indicator + reconciliation with the daemon's own pending lists
  useEffect(() => {
    if (!client) {
      return undefined;
    }
    let stopped = false;
    const tick = async () => {
      const startedAt = Date.now();
      try {
        const [h, approvals, runList] = await Promise.all([
          client.health(),
          client.listApprovals(),
          client.listRuns()
        ]);
        if (stopped) {
          return;
        }
        setHealth(h);
        setRuns(runList);
        dispatch({
          type: "reconcile",
          approvals: new Set(approvals.map((a) => a.call_id)),
          live: new Set(
            runList.filter((r) => LIVE_RUN_STATES.has(r.state)).map((r) => r.id)
          ),
          asOfMs: startedAt - 1500
        });
      } catch {
        if (!stopped) {
          setHealth(null);
        }
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), 3000);
    return () => {
      stopped = true;
      window.clearInterval(id);
    };
  }, [client]);

  // 4. keep the lists fresh when runs start / finish
  const lastRunEvents = useRef(0);
  useEffect(() => {
    const total = Object.keys(events.runs).length;
    const finished = Object.values(events.runs).filter(
      (r) => r.finishedMs !== undefined
    ).length;
    const signature = total * 10_000 + finished;
    if (signature !== lastRunEvents.current && client) {
      lastRunEvents.current = signature;
      void Promise.all([refreshChats(), refreshRuns()]).catch(() => {});
    }
  }, [events.runs, client, refreshChats, refreshRuns]);

  const retry = useCallback(() => setAttempt((n) => n + 1), []);
  const restartDaemon = useCallback(async () => {
    setPhase("connecting");
    try {
      const c = await connectDaemon(true);
      setConn(c);
      setPhase("ready");
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setPhase("error");
    }
  }, []);
  const recordAnswer = useCallback(
    (runId: string, callId: string, answer: string) =>
      dispatch({ type: "answered", runId, callId, answer }),
    []
  );

  const needs = useMemo(() => needsYou(events), [events]);

  const value: DaemonContextValue = {
    phase,
    error,
    conn,
    client,
    health,
    eventsConnected,
    bots,
    chats,
    runs,
    events,
    needs,
    refreshBots,
    refreshChats,
    refreshRuns,
    recordAnswer,
    retry,
    restartDaemon,
    onDaemonEvent
  };
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
