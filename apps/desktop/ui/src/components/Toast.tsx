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
import { CircleAlert, CircleCheck, Info } from "lucide-react";
import { toastDuration, type ToastTone } from "../lib/toastTiming";
import styles from "./Toast.module.css";

export type { ToastTone };


interface ToastItem {
  id: number;
  text: string;
  tone: ToastTone;
}

interface ToastApi {
  show: (text: string, tone?: ToastTone) => void;
}

const ICONS = { info: Info, success: CircleCheck, danger: CircleAlert } as const;


const Ctx = createContext<ToastApi>({ show: () => {} });

export function useToast(): ToastApi {
  return useContext(Ctx);
}

/** Small confirmation toasts (bottom right, live region, auto-dismiss). */
export function ToastProvider({ children }: { children: ReactNode }) {
  const [items, setItems] = useState<ToastItem[]>([]);
  const nextId = useRef(1);
  const timers = useRef(new Set<number>());

  const show = useCallback((text: string, tone: ToastTone = "info") => {
    const id = nextId.current++;
    setItems((prev) => [...prev.slice(-3), { id, text, tone }]);
    const timer = window.setTimeout(() => {
      timers.current.delete(timer);
      setItems((prev) => prev.filter((i) => i.id !== id));
    }, toastDuration(tone));
    timers.current.add(timer);
  }, []);

  useEffect(() => {
    const pending = timers.current;
    return () => {
      for (const t of pending) {
        window.clearTimeout(t);
      }
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
            <span>{i.text}</span>
          </div>
        ))}
      </div>
    </Ctx.Provider>
  );
}
