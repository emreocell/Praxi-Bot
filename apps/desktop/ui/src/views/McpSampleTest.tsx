import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "ui";
import type { ApiClient } from "../api/client";
import type { McpTestResult } from "../api/types";
import { testFailure } from "../lib/googleSetup";
import { humanError } from "../lib/humanError";
import own from "./McpSignInPanel.module.css";

/**
 * The app's own "Test" button: one fixed read-only call, the first three items it returns. The
 * answer is shown here only; it never goes to a bot.
 */
export function McpSampleTest({
  client,
  name,
  disabled,
  autoRun
}: {
  client: Pick<ApiClient, "testMcpServer"> | null;
  name: string;
  disabled: boolean;
  /** Run the test once on mount (right after a sign-in finished). */
  autoRun?: boolean;
}) {
  const { t } = useTranslation();
  const [state, setState] = useState<
    { step: "idle" } | { step: "running" } | { step: "ok"; result: McpTestResult } | { step: "failed"; reason: string; detail: string }
  >({ step: "idle" });
  const shown = state;
  const run = async () => {
    if (!client) return;
    setState({ step: "running" });
    try {
      setState({ step: "ok", result: await client.testMcpServer(name) });
    } catch (e) {
      setState({ step: "failed", reason: testFailure(e), detail: humanError(e).details ?? "" });
    }
  };
  useEffect(() => {
    if (autoRun) void run();
    // Once, when it appears.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return (
    <div data-testid={`mcp-sample-${name}`}>
      <Button size="sm" variant="secondary" disabled={disabled || !client || shown.step === "running"} onClick={() => void run()}>
        {shown.step === "running" ? t("apps.sample.running") : t("apps.sample.run")}
      </Button>
      {shown.step === "ok" ? (
        <div role="status">
          {shown.result.items.length > 0 ? (
            <>
              <p className={own.text}>{t("apps.sample.ok", { n: shown.result.items.length })}</p>
              <ul className={own.items}>
                {shown.result.items.map((item, i) => (
                  <li key={i}>{item}</li>
                ))}
              </ul>
              {shown.result.more ? <p className={own.text}>{t("apps.sample.more")}</p> : null}
            </>
          ) : shown.result.text ? (
            <>
              <p className={own.text}>{t("apps.sample.okText")}</p>
              <p className={own.items}>{shown.result.text}</p>
            </>
          ) : (
            <p className={own.text}>{t("apps.sample.empty")}</p>
          )}
        </div>
      ) : null}
      {shown.step === "failed" ? (
        <div role="alert">
          <p className={`${own.text} ${own.bad}`}>{t(`apps.sample.failed.${shown.reason}`)}</p>
          <details className={own.details}>
            <summary>{t("apps.signIn.details")}</summary>
            <pre>{shown.detail}</pre>
          </details>
        </div>
      ) : null}
    </div>
  );
}
