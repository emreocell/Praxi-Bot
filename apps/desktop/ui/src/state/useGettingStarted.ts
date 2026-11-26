import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { hasRealModel } from "../demo/demoEngine";
import { approvalDecided, onApprovalDecided } from "../lib/approvalTried";
import { botsAsk, buildSteps, countDecided, type Signals, type Step } from "../lib/gettingStarted";
import { useDaemon } from "./DaemonContext";
import { useMcp } from "./useMcp";
import { useProviders } from "./useProviders";

type Counts = Pick<Signals, "runCount" | "decidedApprovals" | "scheduleCount" | "serverCount">;

const NO_COUNTS: Counts = { runCount: null, decidedApprovals: null, scheduleCount: null, serverCount: null };

function pick<T>(r: PromiseSettledResult<T>, f: (v: T) => number): number | null {
  return r.status === "fulfilled" ? f(r.value) : null;
}

/**
 * The checklist steps, each one done or not according to what the app really has: a connected
 * model, a run, a decided approval, a connected app, a schedule, a paired server. A call that
 * fails leaves its step open; nothing is ever marked done on a guess.
 */
export function useGettingStarted(): Step[] {
  const { i18n } = useTranslation();
  const { client, bots } = useDaemon();
  const [decidedHere, setDecidedHere] = useState(approvalDecided);
  useEffect(() => onApprovalDecided(() => setDecidedHere(true)), []);
  const { dto } = useProviders();
  const { servers } = useMcp(true);
  const [counts, setCounts] = useState<Counts>(NO_COUNTS);
  useEffect(() => {
    if (!client) {
      return undefined;
    }
    let live = true;
    void Promise.allSettled([client.listRuns(), client.listApprovals(), client.listSchedules(), client.listHosts()]).then(
      ([runs, approvals, schedules, hosts]) => {
        if (!live) return;
        setCounts({
          // Runs a schedule started on its own are not "your first message".
          runCount: pick(runs, (v) => v.filter((r) => !r.schedule_id).length),
          // The daemon lists only waiting approvals without a run id, so a decision made in
          // this app also counts (see approvalTried.ts).
          decidedApprovals: pick(approvals, countDecided),
          scheduleCount: pick(schedules, (v) => v.length),
          serverCount: pick(hosts, (v) => v.hosts.length)
        });
      }
    );
    return () => {
      live = false;
    };
  }, [client]);
  return useMemo(() => {
    const signals: Signals = {
      ...counts,
      decidedApprovals: decidedHere ? Math.max(1, counts.decidedApprovals ?? 0) : counts.decidedApprovals,
      botsAsk: botsAsk(bots),
      // Not loaded yet is "unknown", not "connected" (hasRealModel treats null as connected).
      hasModel: dto === null ? null : hasRealModel(dto),
      appCount: servers.filter((s) => s.status === "connected").length
    };
    return buildSteps(i18n.language, signals);
  }, [counts, dto, servers, i18n.language, decidedHere, bots]);
}
