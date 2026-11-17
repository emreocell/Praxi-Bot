import { type ReactNode, createContext, useCallback, useContext, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { ServerTerminal } from "../components/ServerTerminal";
import { useDaemon } from "./DaemonContext";

interface ServerTerminalValue {
  /** A server is the bots' home, so there is a shell to open on it. */
  available: boolean;
  /** Open the terminal; `command` is typed into the input line, never run. */
  open: (command?: string) => void;
}

const Ctx = createContext<ServerTerminalValue>({ available: false, open: () => {} });

export function useServerTerminal(): ServerTerminalValue {
  return useContext(Ctx);
}

/**
 * The in-app terminal of the server that is the bots' home: one panel for the whole app, opened
 * from the status-chip menu and from every "run this on the server" hint.
 */
export function ServerTerminalProvider({ children }: { children: ReactNode }) {
  const { home, localApi } = useDaemon();
  const [state, setState] = useState<{ open: boolean; command: string | null }>({ open: false, command: null });
  const available = home.kind === "host" && localApi !== null;
  const open = useCallback((command?: string) => setState({ open: true, command: command ?? null }), []);
  const value = useMemo(() => ({ available, open }), [available, open]);
  return (
    <Ctx.Provider value={value}>
      {children}
      {state.open && home.kind === "host" && localApi
        ? // Outside `#root`: a dialog that opens later makes the app inert, not the shell.
          createPortal(
            <ServerTerminal
              api={localApi}
              hostId={home.hostId}
              hostName={home.name}
              prefill={state.command}
              onClose={() => setState({ open: false, command: null })}
            />,
            document.body
          )
        : null}
    </Ctx.Provider>
  );
}
