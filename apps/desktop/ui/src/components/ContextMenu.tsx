import {
  Archive,
  ArchiveRestore,
  ArrowUpRight,
  Bell,
  BellOff,
  Check,
  Copy,
  Download,
  ExternalLink,
  File as FileIcon,
  FileText,
  Image as ImageIcon,
  Info,
  Link as LinkIcon,
  MessagesSquare,
  Pencil,
  Pin,
  PinOff,
  Quote,
  RefreshCw,
  Search,
  SlidersHorizontal,
  Sparkles,
  Star,
  Trash2,
  UserRound,
  Volume2,
  type LucideIcon
} from "lucide-react";
import {
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type MouseEvent as ReactMouseEvent,
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState
} from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import {
  type ActionItem,
  FRESH_TYPEAHEAD,
  LONG_PRESS_MS,
  type MenuIcon,
  type MenuSpec,
  type Placement,
  type Translate,
  focusableIndexes,
  hasActions,
  isMenuKey,
  isSafeToRun,
  keepNativeMenu,
  navigate,
  placeMenu,
  pressMoved
} from "../lib/contextMenu";
import { layerStack } from "../lib/modalStack";
import { popoverEscapeAction, popoverPressAction } from "../lib/popoverLayers";
import styles from "./ContextMenu.module.css";

const ICONS: Record<MenuIcon, LucideIcon> = {
  copy: Copy,
  markdown: FileText,
  quote: Quote,
  sparkles: Sparkles,
  search: Search,
  searchAll: MessagesSquare,
  edit: Pencil,
  refresh: RefreshCw,
  speak: Volume2,
  info: Info,
  external: ExternalLink,
  link: LinkIcon,
  image: ImageIcon,
  download: Download,
  file: FileIcon,
  open: ArrowUpRight,
  pin: Pin,
  pinOff: PinOff,
  archive: Archive,
  unarchive: ArchiveRestore,
  trash: Trash2,
  bell: Bell,
  bellOff: BellOff,
  star: Star,
  user: UserRound,
  settings: SlidersHorizontal
};

/** Elements whose own (native) menu must stay: text input, spell check, paste and dictation live there. */
const NATIVE_ZONE = 'input,textarea,select,[contenteditable=""],[contenteditable="true"],[data-native-menu]';

/** Where a menu opens: a point (pointer) or the focused element (keyboard). */
interface MenuRequest {
  spec: MenuSpec;
  x: number;
  y: number;
  origin: HTMLElement | null;
  via: "pointer" | "keyboard";
}

/** One menu open at a time across the whole app. */
let closeActive: (() => void) | null = null;

/** What a `bind()` call asks of its owner: the menu for the thing under the pointer, or null for "use the native one". */
export type ResolveMenu = (target: HTMLElement, via: "pointer" | "keyboard") => MenuSpec | null;

/** `t` in the shape the menu builders take (labels are translated when the menu is built). */
export function useMenuTranslate(): Translate {
  const { t } = useTranslation();
  return useCallback((key, vars) => String(t(key, vars as never)), [t]);
}

/** Ctrl on Windows and Linux, Cmd on macOS (for shortcut hints). */
export function modKeyLabel(): string {
  return typeof navigator !== "undefined" && /mac|iphone|ipad/i.test(navigator.platform || navigator.userAgent) ? "⌘" : "Ctrl";
}

/**
 * Right-click menus.  `bind(resolve)` returns the handlers for one area (`onContextMenu`, Shift+F10 /
 * the Menu key, long press); render `menu` once next to it.  The browser's own menu stays for text
 * fields and for Shift+right-click, and wherever `resolve` returns null.
 */
export function useContextMenu() {
  const [request, setRequest] = useState<MenuRequest | null>(null);
  const press = useRef<{ timer: number; x: number; y: number } | null>(null);
  const closer = useCallback(() => setRequest(null), []);

  const close = useCallback((restoreFocus: boolean) => {
    setRequest((r) => {
      if (r && restoreFocus && r.origin?.isConnected) r.origin.focus({ preventScroll: true });
      return null;
    });
  }, []);

  const openAt = useCallback(
    (spec: MenuSpec, x: number, y: number, origin: HTMLElement | null, via: "pointer" | "keyboard") => {
      if (!hasActions(spec.items)) return;
      closeActive?.();
      closeActive = closer;
      setRequest({ spec, x, y, origin, via });
    },
    [closer]
  );

  useEffect(() => () => {
    if (closeActive === closer) closeActive = null;
  }, [closer]);

  const bind = useCallback(
    (resolve: ResolveMenu) => ({
      onContextMenu(e: ReactMouseEvent<HTMLElement>) {
        const target = e.target as HTMLElement;
        if (!target?.closest) return;
        const native = keepNativeMenu({
          shiftKey: e.shiftKey,
          tagName: target.tagName ?? "",
          isContentEditable: target.isContentEditable === true,
          nativeZone: target.closest(NATIVE_ZONE) !== null
        });
        if (native) return;
        const spec = resolve(target, "pointer");
        if (!spec || !hasActions(spec.items)) return;
        e.preventDefault();
        e.stopPropagation();
        openAt(spec, e.clientX, e.clientY, target.closest<HTMLElement>("button,a,[tabindex]") ?? target, "pointer");
      },
      onKeyDown(e: ReactKeyboardEvent<HTMLElement>) {
        if (!isMenuKey(e.nativeEvent)) return;
        const target = e.target as HTMLElement;
        if (!target?.closest || target.closest(NATIVE_ZONE)) return;
        const spec = resolve(target, "keyboard");
        if (!spec || !hasActions(spec.items)) return;
        e.preventDefault();
        e.stopPropagation();
        const r = target.getBoundingClientRect();
        openAt(spec, r.left + Math.min(r.width / 2, 24), r.top + Math.min(r.height, 28), target, "keyboard");
      },
      // Touch and pen: a long press opens the menu (a mouse never long-presses).
      onPointerDown(e: ReactPointerEvent<HTMLElement>) {
        if (e.pointerType === "mouse") return;
        const target = e.target as HTMLElement;
        if (!target?.closest || target.closest(NATIVE_ZONE)) return;
        const { clientX: x, clientY: y } = e;
        window.clearTimeout(press.current?.timer);
        press.current = {
          x,
          y,
          timer: window.setTimeout(() => {
            press.current = null;
            const spec = resolve(target, "pointer");
            if (spec) openAt(spec, x, y, target, "pointer");
          }, LONG_PRESS_MS)
        };
      },
      onPointerMove(e: ReactPointerEvent<HTMLElement>) {
        const p = press.current;
        if (p && pressMoved(e.clientX - p.x, e.clientY - p.y)) {
          window.clearTimeout(p.timer);
          press.current = null;
        }
      },
      onPointerUp() {
        window.clearTimeout(press.current?.timer);
        press.current = null;
      },
      onPointerCancel() {
        window.clearTimeout(press.current?.timer);
        press.current = null;
      }
    }),
    [openAt]
  );

  const menu = request ? <ContextMenu key={`${request.x}:${request.y}:${request.spec.label}`} request={request} onClose={close} /> : null;
  return { bind, menu, close: () => close(false) };
}

function ContextMenu({ request, onClose }: { request: MenuRequest; onClose: (restoreFocus: boolean) => void }) {
  const root = useRef<HTMLDivElement | null>(null);
  const [place, setPlace] = useState<Placement | null>(null);
  const seqRef = useRef(0);
  if (seqRef.current === 0) seqRef.current = layerStack.next();
  const typeahead = useRef(FRESH_TYPEAHEAD);
  const { items, label } = request.spec;

  useLayoutEffect(() => {
    const el = root.current;
    if (!el) return;
    setPlace(
      placeMenu({
        x: request.x,
        y: request.y,
        width: el.offsetWidth,
        height: el.offsetHeight,
        viewportWidth: window.innerWidth,
        viewportHeight: window.innerHeight
      })
    );
  }, [request]);

  useEffect(() => {
    const seq = seqRef.current;
    layerStack.add(seq);
    const el = root.current;
    if (request.via === "keyboard") {
      const first = focusableIndexes(items)[0];
      (el?.querySelector<HTMLElement>(`[data-idx="${first}"]`) ?? el)?.focus({ preventScroll: true });
    } else {
      el?.focus({ preventScroll: true });
    }
    const outside = (target: EventTarget | null) => !(target instanceof Node && el?.contains(target));
    const onDown = (e: MouseEvent) => {
      if (popoverPressAction(layerStack, seq, !outside(e.target)) === "close") onClose(false);
    };
    const onContext = (e: MouseEvent) => {
      if (outside(e.target) && layerStack.isTop(seq)) onClose(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (popoverEscapeAction(layerStack, seq, e.key) !== "close") return;
      e.preventDefault();
      e.stopPropagation();
      onClose(true);
    };
    // A scroll the person starts closes the menu; the page scrolling by itself (a streaming answer
    // following its end) does not.
    const onWheel = (e: Event) => {
      if (outside(e.target)) onClose(false);
    };
    const dismiss = () => onClose(false);
    const watch = window.setInterval(() => {
      if (request.origin && !request.origin.isConnected) onClose(false);
    }, 400);
    document.addEventListener("mousedown", onDown, true);
    document.addEventListener("contextmenu", onContext, true);
    document.addEventListener("keydown", onKey, true);
    document.addEventListener("wheel", onWheel, { capture: true, passive: true });
    document.addEventListener("touchmove", onWheel, { capture: true, passive: true });
    window.addEventListener("resize", dismiss);
    window.addEventListener("blur", dismiss);
    window.addEventListener("popstate", dismiss);
    window.addEventListener("hashchange", dismiss);
    return () => {
      window.clearInterval(watch);
      document.removeEventListener("mousedown", onDown, true);
      document.removeEventListener("contextmenu", onContext, true);
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("wheel", onWheel, true);
      document.removeEventListener("touchmove", onWheel, true);
      window.removeEventListener("resize", dismiss);
      window.removeEventListener("blur", dismiss);
      window.removeEventListener("popstate", dismiss);
      window.removeEventListener("hashchange", dismiss);
      layerStack.remove(seq);
    };
  }, [request, items, onClose]);

  const focusIndex = (i: number) => root.current?.querySelector<HTMLElement>(`[data-idx="${i}"]`)?.focus({ preventScroll: true });
  const current = () => {
    const at = (document.activeElement as HTMLElement | null)?.closest<HTMLElement>("[data-idx]");
    return at ? Number(at.dataset.idx) : -1;
  };

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (!layerStack.isTop(seqRef.current)) return;
    if (e.key === "Tab") {
      e.preventDefault();
      onClose(true);
      return;
    }
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const next = navigate(items, current(), e.key, typeahead.current, e.timeStamp);
    typeahead.current = next.typeahead;
    if (next.index === null) return;
    e.preventDefault();
    focusIndex(next.index);
  };

  const choose = (item: ActionItem) => {
    // A destructive item that does not ask first is refused; a disabled one does nothing.
    if (!isSafeToRun(item)) return;
    onClose(true);
    item.run();
  };

  return createPortal(
    <div
      ref={root}
      className={styles.menu}
      role="menu"
      aria-label={label}
      tabIndex={-1}
      data-testid="context-menu"
      style={{
        left: place?.left ?? 0,
        top: place?.top ?? 0,
        maxHeight: place?.maxHeight,
        // not `visibility: hidden`: a hidden element cannot take focus
        opacity: place ? undefined : 0,
        transformOrigin: place ? `${place.originX} ${place.originY}` : undefined
      }}
      onKeyDown={onKeyDown}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((item, i) => {
        if (item.kind === "separator") return <div key={`s${i}`} className={styles.separator} role="separator" />;
        if (item.kind === "header") {
          return (
            <div key={item.id} className={styles.header} title={item.title} aria-hidden="true">
              {item.label}
            </div>
          );
        }
        const Icon = item.icon ? ICONS[item.icon] : null;
        const checkbox = item.checked !== undefined;
        return (
          <button
            key={item.id}
            type="button"
            role={checkbox ? "menuitemcheckbox" : "menuitem"}
            aria-checked={checkbox ? item.checked : undefined}
            aria-disabled={item.disabled || undefined}
            tabIndex={-1}
            className={styles.item}
            data-idx={i}
            data-danger={item.danger ? "true" : undefined}
            data-testid={`context-menu-${item.id}`}
            onPointerEnter={(e) => {
              if (!item.disabled && e.pointerType === "mouse") focusIndex(i);
            }}
            onClick={() => choose(item)}
          >
            <span className={styles.icon} aria-hidden="true">{Icon ? <Icon /> : null}</span>
            <span className={styles.label}>{item.label}</span>
            {item.hint ? <kbd className={styles.hint}>{item.hint}</kbd> : null}
            {checkbox && item.checked ? <Check className={styles.check} aria-hidden="true" /> : null}
          </button>
        );
      })}
    </div>,
    document.body
  );
}
