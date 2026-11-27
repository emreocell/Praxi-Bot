import { createContext, type ReactNode, useContext, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ApiClient } from "../api/client";
import { type DocCitation, openCitation, parseCitationHref } from "../lib/documentCitation";
import { humanLine } from "../lib/humanError";
import { ConfirmDialog } from "./DialogParts";
import { useToast } from "./Toast";
import styles from "./DocCitationLink.module.css";

/** What a chat message needs to open a citation: the opener, and the name of the folder a link points into. */
export interface CitationHandler {
  open: (citation: DocCitation) => void;
  /** The label of one of this bot's folders; `null` when the link names a folder the bot does not have. */
  sourceLabel: (sourceId: string) => Promise<string | null>;
}

/** Who opens a citation: provided by the chat message that knows its bot and the daemon client. */
const OpenCitationContext = createContext<CitationHandler | null>(null);
export const DocCitationProvider = OpenCitationContext.Provider;

/** The handler for one bot's answers; `null` (links stay plain text) without a client or bot. */
export function useCitationOpener(client: ApiClient | null, botId: string | null | undefined): CitationHandler | null {
  const { t } = useTranslation();
  const toast = useToast();
  return useMemo(() => {
    if (!client || !botId) return null;
    return {
      open: (citation: DocCitation) => {
        void openCitation(client, botId, citation).then(
          (result) => toast.show(t(result === "viewed" ? "documents.citation.viewed" : "documents.citation.saved", { file: citation.fileName }), "success"),
          (error: unknown) => toast.show(`${t("documents.citation.failed", { file: citation.fileName })} (${humanLine(error)})`, "danger")
        );
      },
      sourceLabel: async (sourceId: string) => (await client.listKnowledgeSources(botId)).find((source) => source.id === sourceId)?.label ?? null
    };
  }, [client, botId, toast, t]);
}

/**
 * A `praxi-doc://` link in an answer: a button that, once the owner has seen which folder and file it
 * names, opens that file at that page. Any message or document can contain such a link, so a click
 * never opens anything by itself.
 */
export function DocCitationLink({ href, children }: { href: string; children: ReactNode }) {
  const { t } = useTranslation();
  const handler = useContext(OpenCitationContext);
  const citation = parseCitationHref(href);
  const [asking, setAsking] = useState(false);
  /** `undefined` while the folder name loads, `null` when the bot has no such folder. */
  const [label, setLabel] = useState<string | null | undefined>(undefined);
  const sourceId = citation?.sourceId;
  useEffect(() => {
    if (!asking || !handler || !sourceId) return;
    let live = true;
    setLabel(undefined);
    handler.sourceLabel(sourceId).then(
      (found) => live && setLabel(found),
      () => live && setLabel(null)
    );
    return () => {
      live = false;
    };
  }, [asking, handler, sourceId]);
  if (!citation || !handler) return <span className={styles.plain}>{children}</span>;
  return (
    <>
      <button type="button" className={styles.citation} title={t("documents.citation.open", { file: citation.locator })} onClick={() => setAsking(true)}>
        {children}
      </button>
      <ConfirmDialog
        open={asking}
        title={t("documents.citation.confirmTitle")}
        body={
          <>
            <p>{t("documents.citation.confirmBody")}</p>
            <p data-testid="citation-source">
              {label === undefined ? t("documents.citation.sourceLoading") : label === null ? t("documents.citation.sourceUnknown") : t("documents.citation.source", { label })}
            </p>
            <p data-testid="citation-file">{t("documents.citation.file", { file: citation.locator })}</p>
            {citation.page ? <p>{t("documents.citation.page", { page: citation.page })}</p> : null}
          </>
        }
        confirmLabel={t("documents.citation.confirm")}
        confirmDisabled={typeof label !== "string"}
        onConfirm={() => {
          setAsking(false);
          handler.open(citation);
        }}
        onCancel={() => setAsking(false)}
      />
    </>
  );
}
