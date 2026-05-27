import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, fireEvent } from "@testing-library/react";
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
});
