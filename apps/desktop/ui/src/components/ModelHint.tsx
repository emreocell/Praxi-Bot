import { Lightbulb } from "lucide-react";
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
import styles from "./ModelHint.module.css";

const OPEN_DELAY_MS = 120;
const CLOSE_DELAY_MS = 180;

/**
 * "A light model is set ..." as a small lightbulb beside the model name instead of a permanent box.
 * The text and a "Change model" link show on mouse hover, keyboard focus or a tap (a click pins it
 * open); Escape or a press elsewhere closes it. The text stays in the page, so screen readers get it
 * as the button's description. The panel is drawn inside the nearest `[data-hint-boundary]` so a
 * narrow right panel never clips it.
 */
export function ModelHint({ onChangeModel }: { onChangeModel: () => void }) {
  const { t } = useTranslation();
  const id = useId();
  const root = useRef<HTMLSpanElement | null>(null);
  const button = useRef<HTMLButtonElement | null>(null);
  const pop = useRef<HTMLDivElement | null>(null);
  const timer = useRef<number | undefined>(undefined);
  const [hover, setHover] = useState(false);
  const [focus, setFocus] = useState(false);
  const [pinned, setPinned] = useState(false);
  const [place, setPlace] = useState<CSSProperties | null>(null);
  const open = hover || focus || pinned;

  const later = (fn: () => void, ms: number) => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(fn, ms);
  };
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const close = useCallback(() => {
    window.clearTimeout(timer.current);
    setHover(false);
    setFocus(false);
    setPinned(false);
  }, []);

  // Keep the panel inside its boundary: as wide as the text wants (up to its own maximum), shifted
  // so it never leaves the boundary, with the caret still under the icon.
  useLayoutEffect(() => {
    const host = root.current;
    const panel = pop.current;
    if (!open || !host || !panel) return undefined;
    const fit = () => {
      const edge = host.closest("[data-hint-boundary]")?.getBoundingClientRect();
      const box = host.getBoundingClientRect();
      const left = edge?.left ?? 0;
      const right = edge?.right ?? window.innerWidth;
      const width = Math.min(panel.offsetWidth, right - left);
      const start = Math.max(left, Math.min(box.left + box.width / 2 - width / 2, right - width));
      setPlace({
        left: start - box.left,
        maxWidth: right - left,
        ["--caret" as string]: `${box.left + box.width / 2 - start}px`
      });
    };
    fit();
    window.addEventListener("resize", fit);
    return () => window.removeEventListener("resize", fit);
  }, [open]);

  useEffect(() => {
    if (!open) return undefined;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      if (pop.current?.contains(document.activeElement)) button.current?.focus();
      close();
    };
    const onPress = (e: PointerEvent) => {
      if (!root.current?.contains(e.target as Node)) close();
    };
    document.addEventListener("keydown", onKey, true);
    document.addEventListener("pointerdown", onPress, true);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("pointerdown", onPress, true);
    };
  }, [open, close]);

  return (
    <span
      ref={root}
      className={styles.root}
      data-open={open ? "true" : "false"}
      // Only a mouse hovers: a touch "enter" fires with the tap and would fight the click toggle.
      onPointerEnter={(e) => e.pointerType === "mouse" && later(() => setHover(true), OPEN_DELAY_MS)}
      onPointerLeave={(e) => e.pointerType === "mouse" && later(() => setHover(false), CLOSE_DELAY_MS)}
      onFocus={(e) => {
        // Keyboard focus opens it; the focus a mouse click or a tap leaves behind does not.
        if (e.target.matches(":focus-visible")) setFocus(true);
      }}
      onBlur={(e) => {
        if (!root.current?.contains(e.relatedTarget as Node | null)) {
          setFocus(false);
          setPinned(false);
        }
      }}
    >
      <button
        ref={button}
        type="button"
        className={styles.button}
        aria-describedby={`${id}-text`}
        aria-expanded={open}
        aria-controls={`${id}-panel`}
        data-testid="bot-panel-lite-hint"
        onClick={() => setPinned((p) => !p)}
      >
        <Lightbulb aria-hidden="true" />
        {/* The name is real text, not a `title`: the panel is the hover hint, a native tooltip would stack on it. */}
        <span className={styles.srOnly}>{t("botPanel.modelHint.label")}</span>
      </button>
      <div
        ref={pop}
        id={`${id}-panel`}
        className={styles.panel}
        role="group"
        aria-label={t("botPanel.modelHint.label")}
        style={place ?? undefined}
        data-testid="bot-panel-lite-hint-panel"
        // A press inside must not blur the icon (that would close the panel under the finger).
        onMouseDown={(e) => e.preventDefault()}
      >
        <p id={`${id}-text`} className={styles.text}>{t("botPanel.liteFullHint")}</p>
        <button
          type="button"
          className={styles.change}
          data-testid="bot-panel-lite-hint-change"
          onClick={() => {
            close();
            onChangeModel();
          }}
        >
          {t("botPanel.modelHint.change")}
        </button>
      </div>
    </span>
  );
}
