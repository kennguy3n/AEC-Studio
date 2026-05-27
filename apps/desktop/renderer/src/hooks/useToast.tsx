/**
 * Toast notification system.
 *
 * Provides `useToast()` which returns `{ toasts, addToast, dismiss }`.
 * Wrap the app in `<ToastProvider>` and render `<ToastContainer />`
 * once inside the shell to display the toast stack.
 *
 * Toast types:
 *   - `success` — auto-dismiss after 5 s
 *   - `info`    — auto-dismiss after 5 s
 *   - `error`   — persists until manually dismissed
 */

import {
  createContext,
  useCallback,
  useContext,
  useRef,
  useState,
} from "react";
import type { ReactNode } from "react";

export type ToastKind = "success" | "info" | "error";

export interface Toast {
  id: string;
  kind: ToastKind;
  message: string;
}

interface ToastContextValue {
  toasts: Toast[];
  addToast: (kind: ToastKind, message: string) => string;
  dismiss: (id: string) => void;
}

const AUTO_DISMISS_MS = 5_000;

const ToastContext = createContext<ToastContextValue | null>(null);

export function useToast(): ToastContextValue {
  const ctx = useContext(ToastContext);
  if (ctx === null) {
    throw new Error("useToast must be used inside a <ToastProvider>");
  }
  return ctx;
}

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const timersRef = useRef<Map<string, ReturnType<typeof setTimeout>>>(
    new Map(),
  );
  // Provider-scoped monotonic counter for toast IDs. Holding the
  // counter in a `useRef` instead of a module-level `let` matters
  // for two reasons: (1) test determinism — vitest tears down and
  // re-mounts the provider for each test, so the per-test sequence
  // starts from 1 instead of leaking ordering across tests run in
  // the same module / sharing the same module instance; (2) HMR
  // safety — a renderer hot-reload in dev that swaps the module
  // would otherwise reset the counter at unpredictable times and
  // could in principle hand out an id collision against a still-
  // mounted toast. Since `ToastContainer` keys on the returned
  // string id directly, a collision would silently drop a toast
  // from the DOM. The ref-scoped counter is bound to the provider's
  // lifecycle, which matches the DOM's keying lifecycle.
  const nextToastIdRef = useRef(1);

  const dismiss = useCallback((id: string) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
    const timer = timersRef.current.get(id);
    if (timer !== undefined) {
      clearTimeout(timer);
      timersRef.current.delete(id);
    }
  }, []);

  const addToast = useCallback(
    (kind: ToastKind, message: string): string => {
      const id = `toast_${nextToastIdRef.current.toString(36)}`;
      nextToastIdRef.current += 1;
      const toast: Toast = { id, kind, message };
      setToasts((prev) => [...prev, toast]);
      if (kind !== "error") {
        const timer = setTimeout(() => {
          dismiss(id);
        }, AUTO_DISMISS_MS);
        timersRef.current.set(id, timer);
      }
      return id;
    },
    [dismiss],
  );

  return (
    <ToastContext.Provider value={{ toasts, addToast, dismiss }}>
      {children}
    </ToastContext.Provider>
  );
}

export function ToastContainer() {
  const { toasts, dismiss } = useToast();
  if (toasts.length === 0) return null;
  return (
    <div
      className="toast-container"
      aria-live="polite"
      data-testid="toast-container"
    >
      {toasts.map((t) => (
        <div
          key={t.id}
          className={`toast toast--${t.kind}`}
          role="alert"
          data-testid={`toast-${t.id}`}
        >
          <span className="toast__message">{t.message}</span>
          <button
            type="button"
            className="toast__dismiss"
            aria-label="Dismiss"
            onClick={() => dismiss(t.id)}
            data-testid={`toast-dismiss-${t.id}`}
          >
            ×
          </button>
        </div>
      ))}
    </div>
  );
}
