import { Terminal } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useServerTerminal } from "../state/ServerTerminalContext";

/**
 * Next to every "run this on the server" command: opens the in-app terminal on that server and
 * types the command into it (the owner reads it and presses Enter; nothing runs by itself).
 * Renders nothing while no server is the bots' home.
 */
export function RunOnServer({
  command,
  className,
  beforeOpen
}: {
  command: string;
  className?: string;
  /** Close the dialog this hint sits in: the terminal must not open behind a modal. */
  beforeOpen?: () => void;
}) {
  const { t } = useTranslation();
  const terminal = useServerTerminal();
  if (!terminal.available) {
    return null;
  }
  return (
    <button
      type="button"
      className={className}
      title={t("terminal.runForMeHint")}
      data-testid="run-on-server"
      onClick={() => {
        beforeOpen?.();
        terminal.open(command);
      }}
    >
      <Terminal aria-hidden="true" />
      {t("terminal.runForMe")}
    </button>
  );
}
