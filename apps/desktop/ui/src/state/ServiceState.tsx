import { createContext, type ReactNode, useContext, useEffect, useMemo, useRef, useState } from "react";
import {
  initialServiceMemory,
  observeService,
  type ServiceMemory,
  serviceView,
  type ServiceView
} from "../lib/serviceState";
import { useDaemon } from "./DaemonContext";

const READY: ServiceView = { status: "ready", bannerVisible: false, blocked: false, canRestart: false };

const ServiceStateContext = createContext<ServiceView>(READY);

/**
 * The app-wide state of the local service, derived from what the daemon context already polls
 * (start-up result + the 3 s health probe). Mount ABOVE any computer switch so it always describes
 * this computer's own service.
 */
export function ServiceStateProvider({ children }: { children: ReactNode }) {
  const { phase, health } = useDaemon();
  const up = phase === "ready" && health !== null;
  const mem = useRef<ServiceMemory>(initialServiceMemory(Date.now()));
  const [now, setNow] = useState(() => Date.now());

  // Record each answer as it arrives (during render: it is a pure fold over the latest input).
  mem.current = observeService(mem.current, up, Date.now());

  // A clock only while the service is down: the quiet period and the 15 s mark are time based.
  const down = mem.current.downSince !== null;
  useEffect(() => {
    if (!down) {
      return undefined;
    }
    setNow(Date.now());
    const id = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(id);
  }, [down]);

  const view = serviceView(mem.current, Math.max(now, Date.now()));
  const stable = useMemo(
    () => view,
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [view.status, view.bannerVisible, view.blocked, view.canRestart]
  );
  return <ServiceStateContext.Provider value={stable}>{children}</ServiceStateContext.Provider>;
}

/** The service state; "ready" outside the provider (isolated tests and previews). */
export function useServiceState(): ServiceView {
  return useContext(ServiceStateContext);
}
