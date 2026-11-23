import { Check, Copy } from "lucide-react";
import { memo, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Token } from "../lib/highlight";
import { whenNearViewport } from "../lib/nearViewport";
import styles from "./Markdown.module.css";

const COPIED_MS = 1500;

/**
 * A fenced code block with a copy button. The highlighter is loaded lazily on
 * near the viewport; offscreen history stays plain, searchable and copyable.
 */
export const CodeBlock = memo(function CodeBlock({
  code,
  lang
}: {
  code: string;
  lang: string;
}) {
  const { t } = useTranslation();
  const [highlighted, setHighlighted] = useState<{ code: string; lang: string; tokens: Token[] } | null>(null);
  const tokens = highlighted?.code === code && highlighted.lang === lang ? highlighted.tokens : null;
  const blockRef = useRef<HTMLDivElement | null>(null);
  const [near, setNear] = useState(false);
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => {
    const block = blockRef.current;
    return block ? whenNearViewport(block, () => setNear(true)) : undefined;
  }, []);

  useEffect(() => {
    if (!near) return;
    let cancelled = false;
    // Coalesce code changes too: never highlight an intermediate token in the same frame.
    const frame = requestAnimationFrame(() => void import("../lib/highlight")
      .then((m) => {
        if (!cancelled) {
          setHighlighted({ code, lang, tokens: m.highlight(code, lang) });
        }
      })
      .catch(() => {}));
    return () => {
      cancelled = true;
      cancelAnimationFrame(frame);
    };
  }, [code, lang, near]);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  const copy = () => {
    void navigator.clipboard
      .writeText(code)
      .then(() => {
        setCopied(true);
        window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => setCopied(false), COPIED_MS);
      })
      .catch(() => {});
  };

  return (
    <div ref={blockRef} className={styles.codeBlock} data-testid="code-block">
      <div className={styles.codeBar}>
        <span className={styles.codeLang}>{lang || t("chat.md.code")}</span>
        <button
          type="button"
          className={styles.copyButton}
          onClick={copy}
          title={t("chat.md.copy")}
          aria-label={t("chat.md.copy")}
        >
          {copied ? <Check /> : <Copy />}{copied ? t("chat.md.copied") : t("chat.md.copy")}
        </button>
      </div>
      <pre className={styles.pre}>
        <code>
          {tokens
            ? tokens.map((tok, i) =>
                tok.type === "plain" ? (
                  tok.text
                ) : (
                  <span key={i} className={styles[`tok_${tok.type}`]}>
                    {tok.text}
                  </span>
                )
              )
            : code}
        </code>
      </pre>
    </div>
  );
});
