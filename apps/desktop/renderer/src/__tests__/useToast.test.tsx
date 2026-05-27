import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, fireEvent } from "@testing-library/react";
import { useEffect, useRef, useState } from "react";
import {
  ToastContainer,
  ToastProvider,
  useToast,
} from "../hooks/useToast";

function Trigger({
  kind,
  message,
}: {
  kind: "success" | "info" | "error";
  message: string;
}) {
  const { addToast } = useToast();
  return (
    <button type="button" onClick={() => addToast(kind, message)}>
      fire
    </button>
  );
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("useToast", () => {
  it("renders a toast and auto-dismisses success after 5s", () => {
    render(
      <ToastProvider>
        <Trigger kind="success" message="saved" />
        <ToastContainer />
      </ToastProvider>,
    );
    fireEvent.click(screen.getByText("fire"));
    expect(screen.getByText("saved")).toBeInTheDocument();
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    expect(screen.queryByText("saved")).not.toBeInTheDocument();
  });

  it("persists error toasts until dismissed", () => {
    render(
      <ToastProvider>
        <Trigger kind="error" message="render failed" />
        <ToastContainer />
      </ToastProvider>,
    );
    fireEvent.click(screen.getByText("fire"));
    expect(screen.getByText("render failed")).toBeInTheDocument();
    act(() => {
      vi.advanceTimersByTime(60_000);
    });
    expect(screen.getByText("render failed")).toBeInTheDocument();
  });

  it("dismisses via the close button", () => {
    render(
      <ToastProvider>
        <Trigger kind="info" message="hello" />
        <ToastContainer />
      </ToastProvider>,
    );
    fireEvent.click(screen.getByText("fire"));
    const dismissBtn = screen.getByRole("button", { name: /dismiss/i });
    fireEvent.click(dismissBtn);
    expect(screen.queryByText("hello")).not.toBeInTheDocument();
  });

  // Regression: Devin Review flagged that `nextToastId` was a
  // module-level `let`, leaking ordering across provider remounts
  // (test order non-determinism, and an HMR collision hazard
  // where a stale toast on the DOM could share an id with a
  // freshly-issued one — `ToastContainer.map(t => <div key={t.id} />)`
  // would silently drop one). The fix promotes the counter to a
  // provider-scoped `useRef`. This test pins the contract:
  // remounting the provider resets the sequence, so each
  // independent provider produces deterministic, non-colliding ids.
  it("issues deterministic ids that reset on provider remount", () => {
    const seen: string[] = [];
    function Capture() {
      const { addToast } = useToast();
      return (
        <button
          type="button"
          onClick={() => seen.push(addToast("info", "x"))}
        >
          fire
        </button>
      );
    }

    const { unmount } = render(
      <ToastProvider>
        <Capture />
      </ToastProvider>,
    );
    fireEvent.click(screen.getByText("fire"));
    fireEvent.click(screen.getByText("fire"));
    fireEvent.click(screen.getByText("fire"));
    expect(seen).toEqual(["toast_1", "toast_2", "toast_3"]);
    unmount();

    // Independent provider instance → counter restarts from 1.
    render(
      <ToastProvider>
        <Capture />
      </ToastProvider>,
    );
    fireEvent.click(screen.getByText("fire"));
    expect(seen[seen.length - 1]).toBe("toast_1");
  });

  // Regression: Devin Review flagged that the context value was an
  // inline object literal `{ toasts, addToast, dismiss }` — fresh
  // reference on every render. Now memoised with deps
  // `[toasts, addToast, dismiss]` so any consumer reading the full
  // value object via `useToast()` sees a stable reference across
  // renders that don't actually change the toast list.
  //
  // The contract this test pins: when the toast list is unchanged,
  // re-rendering a sibling that triggers a `ToastProvider` parent
  // re-render must NOT hand consumers a new context value.
  it("memoises the context value across no-op parent re-renders", () => {
    const seen: unknown[] = [];
    function Capture() {
      const ctx = useToast();
      const last = useRef<unknown>(null);
      if (last.current !== ctx) {
        seen.push(ctx);
        last.current = ctx;
      }
      return null;
    }

    function Outer() {
      // `tick` re-renders the provider without touching toast
      // state. A non-memoised provider would issue a new context
      // value on every tick; the memoised provider must not.
      const [tick, setTick] = useState(0);
      return (
        <ToastProvider>
          <Capture />
          <button type="button" onClick={() => setTick(tick + 1)}>
            tick
          </button>
        </ToastProvider>
      );
    }

    render(<Outer />);
    expect(seen.length).toBe(1);
    fireEvent.click(screen.getByText("tick"));
    fireEvent.click(screen.getByText("tick"));
    fireEvent.click(screen.getByText("tick"));
    expect(seen.length).toBe(1);
  });

  // Regression: Devin Review flagged that auto-dismiss timers in
  // `timersRef` were never cleaned up on provider unmount, so a
  // `setTimeout` armed by `addToast` would fire AFTER the provider
  // was torn down — calling `dismiss(id)` and `setToasts(...)` on a
  // dead component. The fix is a dedicated `useEffect` whose
  // cleanup iterates the map and `clearTimeout`s every entry.
  //
  // The contract this test pins: unmounting the provider while
  // multiple auto-dismiss toasts are in flight clears every pending
  // timer, so advancing fake timers past AUTO_DISMISS_MS never
  // dispatches the queued callbacks.
  it("clears every pending auto-dismiss timer on provider unmount", () => {
    const clearSpy = vi.spyOn(globalThis, "clearTimeout");
    function ArmThree() {
      const { addToast } = useToast();
      useEffect(() => {
        addToast("info", "a");
        addToast("success", "b");
        addToast("info", "c");
      }, [addToast]);
      return null;
    }

    const { unmount } = render(
      <ToastProvider>
        <ArmThree />
        <ToastContainer />
      </ToastProvider>,
    );
    expect(screen.getByText("a")).toBeInTheDocument();
    expect(screen.getByText("b")).toBeInTheDocument();
    expect(screen.getByText("c")).toBeInTheDocument();
    const callsBeforeUnmount = clearSpy.mock.calls.length;

    unmount();

    // Three pending auto-dismiss timers must be cleared on unmount.
    const callsAfterUnmount = clearSpy.mock.calls.length;
    expect(callsAfterUnmount - callsBeforeUnmount).toBeGreaterThanOrEqual(3);

    // Advancing past AUTO_DISMISS_MS after unmount must not throw
    // (no stale dispatch into a torn-down state). React's act()
    // would surface a "state update on unmounted component" warning
    // if any timer survived.
    act(() => {
      vi.advanceTimersByTime(10_000);
    });
    clearSpy.mockRestore();
  });
});
