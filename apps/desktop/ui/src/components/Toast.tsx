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
import styles from "./Toast.module.css";

export type ToastTone = "info" | "success" | "danger";

interface ToastItem {
  id: number;
  text: string;
  tone: ToastTone;
}

interface ToastApi {
  show: (text: string, tone?: ToastTone) => void;
}

const TOAST_MS = 4000;

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
    }, TOAST_MS);
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
          >
            {i.text}
          </div>
        ))}
      </div>
    </Ctx.Provider>
  );
}
