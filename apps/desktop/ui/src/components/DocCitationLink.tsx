import { createContext, type ReactNode, useCallback, useContext } from "react";
import { useTranslation } from "react-i18next";
import type { ApiClient } from "../api/client";
import { type DocCitation, openCitation, parseCitationHref } from "../lib/documentCitation";
import { humanLine } from "../lib/humanError";
import { useToast } from "./Toast";
import styles from "./DocCitationLink.module.css";

/** Who opens a citation: provided by the chat message that knows its bot and the daemon client. */
const OpenCitationContext = createContext<((citation: DocCitation) => void) | null>(null);
export const DocCitationProvider = OpenCitationContext.Provider;

/** The handler for one bot's answers; `null` (links stay plain text) without a client or bot. */
export function useCitationOpener(client: ApiClient | null, botId: string | null | undefined): ((citation: DocCitation) => void) | null {
  const { t } = useTranslation();
  const toast = useToast();
  const open = useCallback(
    (citation: DocCitation) => {
      if (!client || !botId) return;
      void openCitation(client, botId, citation).then(
        (result) => toast.show(t(result === "viewed" ? "documents.citation.viewed" : "documents.citation.saved", { file: citation.fileName }), "success"),
        (error: unknown) => toast.show(`${t("documents.citation.failed", { file: citation.fileName })} (${humanLine(error)})`, "danger")
      );
    },
    [client, botId, toast, t]
  );
  return client && botId ? open : null;
}

/** A `praxi-doc://` link in an answer: a button that opens that file at that page. */
export function DocCitationLink({ href, children }: { href: string; children: ReactNode }) {
  const { t } = useTranslation();
  const open = useContext(OpenCitationContext);
  const citation = parseCitationHref(href);
  if (!citation || !open) return <span className={styles.plain}>{children}</span>;
  return (
    <button
      type="button"
      className={styles.citation}
      title={t("documents.citation.open", { file: citation.locator })}
      onClick={() => open(citation)}
    >
      {children}
    </button>
  );
}
