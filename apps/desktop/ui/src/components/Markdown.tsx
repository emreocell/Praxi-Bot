import { type CSSProperties, Fragment, memo, type ReactNode, useMemo } from "react";
import { openExternal } from "../lib/externalLink";
import {
  type Align,
  type Block,
  type Inline,
  parseMarkdown
} from "../lib/markdown";
import { CodeBlock } from "./CodeBlock";
import styles from "./Markdown.module.css";

function renderInline(nodes: Inline[]): ReactNode {
  return nodes.map((n, i) => {
    switch (n.t) {
      case "text":
        return <Fragment key={i}>{n.text}</Fragment>;
      case "br":
        return <br key={i} />;
      case "code":
        return (
          <code key={i} className={styles.inlineCode}>
            {n.text}
          </code>
        );
      case "strong":
        return <strong key={i}>{renderInline(n.children)}</strong>;
      case "em":
        return <em key={i}>{renderInline(n.children)}</em>;
      case "del":
        return <del key={i}>{renderInline(n.children)}</del>;
      case "link":
        return (
          <a
            key={i}
            className={styles.link}
            href={n.href}
            target="_blank"
            rel="noopener noreferrer nofollow"
            title={n.href}
            onClick={(e) => {
              // links always go to the system browser, never the app window
              e.preventDefault();
              openExternal(n.href);
            }}
          >
            {renderInline(n.children)}
          </a>
        );
    }
  });
}

const HEADINGS = ["h1", "h2", "h3", "h4", "h5", "h6"] as const;

function cellStyle(align: Align): CSSProperties | undefined {
  return align ? { textAlign: align } : undefined;
}

function renderBlocks(blocks: Block[]): ReactNode {
  return blocks.map((b, i) => {
    switch (b.t) {
      case "p":
        return <p key={i}>{renderInline(b.children)}</p>;
      case "heading": {
        const Tag = HEADINGS[Math.min(b.level, 6) - 1] ?? "h6";
        return (
          <Tag key={i} className={styles[`h${Math.min(b.level, 4)}`]}>
            {renderInline(b.children)}
          </Tag>
        );
      }
      case "code":
        return <CodeBlock key={i} code={b.text} lang={b.lang} />;
      case "quote":
        return (
          <blockquote key={i} className={styles.quote}>
            {renderBlocks(b.children)}
          </blockquote>
        );
      case "hr":
        return <hr key={i} className={styles.rule} />;
      case "list": {
        const items = b.items.map((it, j) => (
          <li key={j} className={it.checked === null ? undefined : styles.task}>
            {it.checked === null ? null : (
              <input
                type="checkbox"
                checked={it.checked}
                readOnly
                tabIndex={-1}
                aria-hidden="true"
              />
            )}
            <div>{renderBlocks(it.children)}</div>
          </li>
        ));
        return b.ordered ? (
          <ol key={i} start={b.start}>
            {items}
          </ol>
        ) : (
          <ul key={i}>{items}</ul>
        );
      }
      case "table":
        return (
          <div key={i} className={styles.tableWrap}>
            <table className={styles.table}>
              <thead>
                <tr>
                  {b.head.map((h, c) => (
                    <th key={c} style={cellStyle(b.align[c] ?? null)}>
                      {renderInline(h)}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {b.rows.map((row, r) => (
                  <tr key={r}>
                    {row.map((cell, c) => (
                      <td key={c} style={cellStyle(b.align[c] ?? null)}>
                        {renderInline(cell)}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        );
    }
  });
}

/** Renders an assistant message as Markdown (no raw HTML, safe links). */
export const Markdown = memo(function Markdown({ text }: { text: string }) {
  const blocks = useMemo(() => parseMarkdown(text), [text]);
  return (
    <div className={styles.md} data-testid="markdown">
      {renderBlocks(blocks)}
    </div>
  );
});
