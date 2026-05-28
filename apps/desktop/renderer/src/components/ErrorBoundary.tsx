import { Component } from "react";
import type { ErrorInfo, ReactNode } from "react";

interface Props {
  /** What to render when no error has been caught. */
  children: ReactNode;
  /** Human-readable label shown in the fallback (e.g. "BIM page"). */
  label?: string;
  /**
   * Optional retry handler invoked when the "Retry" button is
   * clicked. The boundary always clears its own error state on
   * retry; this hook lets the parent re-fetch data or remount the
   * subtree.
   */
  onRetry?: () => void;
}

interface State {
  error: Error | null;
}

/**
 * Wraps an entire mode page (Design / Draft / BIM / Render /
 * Deliver) so a runtime exception in a deeply-nested component
 * doesn't crash the whole app shell. Surfaces a friendly fallback
 * with the error message + a retry button.
 *
 * Use one per mode page so each can be reset independently. The
 * fallback never includes the stack trace (security: don't leak
 * internal file paths to the user); developers can read it from
 * the devtools console.
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // eslint-disable-next-line no-console
    console.error(
      `[ErrorBoundary${this.props.label ? ` · ${this.props.label}` : ""}]`,
      error,
      info.componentStack,
    );
  }

  reset = (): void => {
    this.setState({ error: null });
    this.props.onRetry?.();
  };

  render(): ReactNode {
    if (this.state.error !== null) {
      return (
        <div
          className="error-boundary"
          role="alert"
          data-testid="error-boundary"
        >
          <h2>Something went wrong{this.props.label ? ` in ${this.props.label}` : ""}.</h2>
          <p>{this.state.error.message}</p>
          <button
            type="button"
            onClick={this.reset}
            data-testid="error-boundary-retry"
          >
            Retry
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
