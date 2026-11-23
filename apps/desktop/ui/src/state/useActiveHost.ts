import { useCallback } from "react";
import { MOCK_BASE_URL } from "../api/mockMode";
import { LOCAL_COMPUTER, useDaemon } from "./DaemonContext";

/**
 * Whether the app is talking to a SERVER (a paired host) right now. There the app is a paired
 * peer: the host refuses its own-computer routes (local models, CLI detection, provider
 * management, connection tests) on purpose, so surfaces must not call them and use the host
 * administration path instead.
 */
export function useActiveHost() {
  const { client, activeComputerId, activeHostName, home, workLocalNow, setWorkLocalNow, selectComputer } =
    useDaemon();
  const mock = client?.baseUrl === MOCK_BASE_URL;
  const isHost = !mock && activeComputerId !== LOCAL_COMPUTER;
  const name = activeHostName || (home.kind === "host" ? home.name : "") || "";
  /** Leave the server and work on this computer (the app would otherwise follow the home back). */
  const switchToThisComputer = useCallback(() => {
    if (home.kind === "host" && !workLocalNow) {
      setWorkLocalNow(true);
    } else {
      void selectComputer(LOCAL_COMPUTER);
    }
  }, [home, workLocalNow, setWorkLocalNow, selectComputer]);
  return { isHost, name, switchToThisComputer };
}
