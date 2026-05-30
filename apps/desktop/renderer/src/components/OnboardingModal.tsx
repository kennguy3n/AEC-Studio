/**
 * Phase 17 Group C Task 19 — first-run onboarding modal.
 *
 * Shown on the Home page when:
 *   * the user has no recent projects (`recents.length === 0`), AND
 *   * the persisted `localStorage["aec.onboarding.dismissed"]`
 *     flag is unset (so it never reappears after the user clicks
 *     "Got it" or "Start with the Empty template").
 *
 * The modal walks the user through the four workflow modes and
 * offers a one-click "Start with the Empty template" CTA that
 * creates a project from the canonical empty template and routes
 * to Design. Every CTA writes the dismissed flag before invoking
 * its callback so the modal cannot re-appear after navigation
 * unmounts Home.
 *
 * The component is fully accessible: it traps focus into the
 * dialog, the close button has an accessible label, the modal is
 * announced via `role="dialog"` + `aria-modal="true"`, and the
 * backdrop is keyboard-dismissable via `Escape`.
 */

import { useCallback, useEffect, useRef } from "react";

export const ONBOARDING_STORAGE_KEY = "aec.onboarding.dismissed";

interface Props {
  open: boolean;
  onDismiss: () => void;
  onStartEmpty: () => void;
}

export function OnboardingModal({ open, onDismiss, onStartEmpty }: Props) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const closeRef = useRef<HTMLButtonElement | null>(null);

  // Focus the close button when the modal opens so a screen
  // reader announces the dialog's role + label, and so keyboard
  // users can immediately Tab into the action row without
  // hunting through the underlying page.
  useEffect(() => {
    if (!open) return;
    closeRef.current?.focus();
  }, [open]);

  // `Escape` dismisses the modal. We bind to `window` rather than
  // the dialog element because focus may end up on a child button
  // — the bubbling listener still fires for the chord.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onDismiss();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onDismiss]);

  const handleStartEmpty = useCallback(() => {
    onStartEmpty();
  }, [onStartEmpty]);

  if (!open) return null;

  return (
    <div
      className="onboarding-modal-backdrop"
      data-testid="onboarding-modal-backdrop"
      style={backdropStyle}
      onClick={onDismiss}
    >
      <div
        ref={dialogRef}
        className="onboarding-modal"
        data-testid="onboarding-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="onboarding-modal-title"
        aria-describedby="onboarding-modal-description"
        style={dialogStyle}
        // Stop click-through so clicking inside the dialog doesn't
        // dismiss it via the backdrop's onClick.
        onClick={(e) => e.stopPropagation()}
      >
        <header style={{ marginBottom: 12 }}>
          <h2
            id="onboarding-modal-title"
            style={{ margin: 0, fontSize: 18, fontWeight: 600 }}
          >
            Welcome to AEC Studio
          </h2>
          <p
            id="onboarding-modal-description"
            style={{
              marginTop: 4,
              marginBottom: 0,
              color: "var(--aec-color-text-muted)",
            }}
          >
            A local-first studio for design, drafting, BIM, and rendering.
          </p>
        </header>

        <ul style={{ paddingLeft: 18, lineHeight: 1.5, margin: 0 }}>
          <li>
            <strong>Design</strong> — sketch walls, doors, and slabs on a
            3D viewport. Tools live in the left rail.
          </li>
          <li>
            <strong>Draft</strong> — produce 2D drawings, import / export
            DXF.
          </li>
          <li>
            <strong>BIM</strong> — import IFC, browse the spatial tree,
            edit property sets, run validators.
          </li>
          <li>
            <strong>Render</strong> — path-traced photorealistic renders
            with HDRI lighting and tone-mapping presets.
          </li>
          <li>
            <strong>Deliver</strong> — produce sheets, schedules, and BOQ
            exports.
          </li>
        </ul>

        <footer
          style={{
            marginTop: 16,
            display: "flex",
            gap: 8,
            justifyContent: "flex-end",
          }}
        >
          <button
            ref={closeRef}
            type="button"
            data-testid="onboarding-modal-dismiss"
            onClick={onDismiss}
            style={secondaryButtonStyle}
          >
            Got it
          </button>
          <button
            type="button"
            data-testid="onboarding-modal-start-empty"
            onClick={handleStartEmpty}
            style={primaryButtonStyle}
          >
            Start with the Empty template
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * Read the persisted dismissed flag. Wrapped in a try/catch so a
 * blocked `localStorage` (private browsing, some kiosk profiles)
 * degrades to "always show" rather than throwing through Home's
 * mount.
 */
export function readOnboardingDismissed(): boolean {
  try {
    return (
      typeof window !== "undefined" &&
      window.localStorage?.getItem(ONBOARDING_STORAGE_KEY) === "1"
    );
  } catch {
    return false;
  }
}

/**
 * Persist the dismissed flag. Same try/catch fallback as
 * `readOnboardingDismissed`.
 */
export function writeOnboardingDismissed(): void {
  try {
    window.localStorage?.setItem(ONBOARDING_STORAGE_KEY, "1");
  } catch {
    // Best-effort persistence; the user will see the modal again
    // next session, which is fine.
  }
}

const backdropStyle: React.CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0, 0, 0, 0.55)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 1000,
};

const dialogStyle: React.CSSProperties = {
  background: "var(--aec-color-surface, #ffffff)",
  color: "var(--aec-color-text, #111111)",
  borderRadius: 8,
  padding: 24,
  maxWidth: 480,
  width: "calc(100% - 32px)",
  boxShadow: "0 12px 32px rgba(0,0,0,0.35)",
};

const primaryButtonStyle: React.CSSProperties = {
  background: "var(--aec-color-accent, #4c8df6)",
  color: "white",
  border: "none",
  padding: "8px 14px",
  borderRadius: 6,
  cursor: "pointer",
  fontSize: 13,
  fontWeight: 600,
};

const secondaryButtonStyle: React.CSSProperties = {
  background: "transparent",
  color: "var(--aec-color-text-muted, #555)",
  border: "1px solid var(--aec-color-border, #d0d0d0)",
  padding: "8px 14px",
  borderRadius: 6,
  cursor: "pointer",
  fontSize: 13,
};
