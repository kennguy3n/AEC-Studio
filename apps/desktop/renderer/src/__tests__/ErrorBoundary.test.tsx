import { describe, expect, it, vi, afterEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { ErrorBoundary } from "../components/ErrorBoundary";

function Boom({ shouldThrow }: { shouldThrow: boolean }) {
  if (shouldThrow) throw new Error("simulated render error");
  return <div data-testid="boom-ok">ok</div>;
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("<ErrorBoundary>", () => {
  it("renders children when no error is thrown", () => {
    render(
      <ErrorBoundary label="Test">
        <Boom shouldThrow={false} />
      </ErrorBoundary>,
    );
    expect(screen.getByTestId("boom-ok")).toBeInTheDocument();
  });

  it("renders the fallback with label + error message on throw", () => {
    // Suppress React's noisy "uncaught error" log for the spec.
    vi.spyOn(console, "error").mockImplementation(() => {});
    render(
      <ErrorBoundary label="BIM">
        <Boom shouldThrow={true} />
      </ErrorBoundary>,
    );
    const alert = screen.getByTestId("error-boundary");
    expect(alert).toHaveTextContent("Something went wrong in BIM");
    expect(alert).toHaveTextContent("simulated render error");
  });

  it("invokes onRetry and resets when Retry is clicked", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    // Use a module-scoped flag so the parent can flip the throw
    // behaviour from inside the onRetry handler — this simulates a
    // real consumer that re-fetches data or remounts the subtree
    // before the boundary re-renders the children.
    let shouldThrow = true;
    const onRetry = vi.fn(() => {
      shouldThrow = false;
    });
    function Subject() {
      return <Boom shouldThrow={shouldThrow} />;
    }
    render(
      <ErrorBoundary label="Render" onRetry={onRetry}>
        <Subject />
      </ErrorBoundary>,
    );
    expect(screen.getByTestId("error-boundary")).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("error-boundary-retry"));
    expect(onRetry).toHaveBeenCalledTimes(1);
    expect(screen.getByTestId("boom-ok")).toBeInTheDocument();
  });
});
