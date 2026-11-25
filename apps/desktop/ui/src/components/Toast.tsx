import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState
} from "react";
import { CircleAlert, CircleCheck, Info, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { toastDuration, type ToastTone } from "../lib/toastTiming";
import styles from "./Toast.module.css";

export type { ToastTone };

export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface ToastOptions {
  action?: ToastAction;
  durationMs?: number;
}

interface ToastItem {
  id: number;
  text: string;
  tone: ToastTone;
  action?: ToastAction;
}

interface ToastApi {
  show: (text: string, tone?: ToastTone, options?: ToastOptions) => void;
}

const ICONS = { info: Info, success: CircleCheck, danger: CircleAlert } as const;

const Ctx = createContext<ToastApi>({ show: () => {} });

export function useToast(): ToastApi {
  return useContext(Ctx);
}

/** Small confirmation toasts (bottom right, live region, auto-dismiss, optional action). */
export function ToastProvider({ children }: { children: ReactNode }) {
  const { t } = useTranslation();
  const [items, setItems] = useState<ToastItem[]>([]);
  const nextId = useRef(1);
  const timers = useRef(new Map<number, number>());

  const dismiss = useCallback((id: number) => {
    const timer = timers.current.get(id);
    if (timer !== undefined) {
      window.clearTimeout(timer);
      timers.current.delete(id);
    }
    setItems((prev) => prev.filter((i) => i.id !== id));
  }, []);

  const show = useCallback(
    (text: string, tone: ToastTone = "info", options?: ToastOptions) => {
      const id = nextId.current++;
      setItems((prev) => [...prev.slice(-3), { id, text, tone, action: options?.action }]);
      const delay = options?.durationMs ?? toastDuration(tone);
      const timer = window.setTimeout(() => {
        timers.current.delete(id);
        setItems((prev) => prev.filter((i) => i.id !== id));
      }, delay);
      timers.current.set(id, timer);
    },
    []
  );

  useEffect(() => {
    const pending = timers.current;
    return () => {
      for (const timer of pending.values()) {
        window.clearTimeout(timer);
      }
      pending.clear();
    };
  }, []);

  const api = useMemo(() => ({ show }), [show]);

  return (
    <Ctx.Provider value={api}>
      {children}
      <div className={styles.region} role="status" aria-live="polite">
        {items.map((i) => (
          <div
            key={i.id}
            className={[styles.toast, styles[i.tone]].join(" ")}
            data-testid="toast"
            role={i.tone === "danger" ? "alert" : undefined}
          >
            {(() => {
              const Icon = ICONS[i.tone];
              return <Icon className={styles.icon} aria-hidden="true" />;
            })()}
            <span className={styles.text}>{i.text}</span>
            {i.action ? (
              <button
                type="button"
                className={styles.actionBtn}
                onClick={() => {
                  i.action?.onClick();
                  dismiss(i.id);
                }}
              >
                {i.action.label}
              </button>
            ) : null}
            <button
              type="button"
              className={styles.closeBtn}
              aria-label={t("notifyHistory.dismiss")}
              title={t("notifyHistory.dismiss")}
              onClick={() => dismiss(i.id)}
            >
              <X aria-hidden="true" />
            </button>
          </div>
        ))}
      </div>
    </Ctx.Provider>
  );
}
