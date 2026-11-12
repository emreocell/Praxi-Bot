import { Check, Copy } from "lucide-react";
import { memo, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Token } from "../lib/highlight";
import styles from "./Markdown.module.css";

const COPIED_MS = 1500;

/**
 * A fenced code block with a copy button. The highlighter is loaded lazily on
 * first use; until then (and for unknown languages) the code is shown plain.
 */
export const CodeBlock = memo(function CodeBlock({
  code,
  lang
}: {
  code: string;
  lang: string;
}) {
  const { t } = useTranslation();
  const [tokens, setTokens] = useState<Token[] | null>(null);
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => {
    let cancelled = false;
    void import("../lib/highlight")
      .then((m) => {
        if (!cancelled) {
          setTokens(m.highlight(code, lang));
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [code, lang]);

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
    <div className={styles.codeBlock} data-testid="code-block">
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
