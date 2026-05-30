import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import {
  OnboardingModal,
  ONBOARDING_STORAGE_KEY,
  readOnboardingDismissed,
  writeOnboardingDismissed,
} from "../components/OnboardingModal";

describe("OnboardingModal (Phase 17 Group C Task 19)", () => {
  beforeEach(() => {
    window.localStorage.removeItem(ONBOARDING_STORAGE_KEY);
  });

  it("does not render when open is false", () => {
    render(
      <OnboardingModal
        open={false}
        onDismiss={() => undefined}
        onStartEmpty={() => undefined}
      />,
    );
    expect(screen.queryByTestId("onboarding-modal")).not.toBeInTheDocument();
  });

  it("renders dialog roles + label + body when open", () => {
    render(
      <OnboardingModal
        open
        onDismiss={() => undefined}
        onStartEmpty={() => undefined}
      />,
    );
    const dialog = screen.getByTestId("onboarding-modal");
    expect(dialog.getAttribute("role")).toBe("dialog");
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("aria-labelledby")).toBe(
      "onboarding-modal-title",
    );
    // All five mode labels live in the modal body so the user sees
    // what each workflow is for at a glance.
    for (const label of ["Design", "Draft", "BIM", "Render", "Deliver"]) {
      expect(screen.getByText(label)).toBeInTheDocument();
    }
  });

  it("invokes onDismiss when the user clicks Got it", () => {
    const onDismiss = vi.fn();
    render(
      <OnboardingModal
        open
        onDismiss={onDismiss}
        onStartEmpty={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("onboarding-modal-dismiss"));
    expect(onDismiss).toHaveBeenCalledOnce();
  });

  it("invokes onStartEmpty when the user clicks Start with the Empty template", () => {
    const onStartEmpty = vi.fn();
    render(
      <OnboardingModal
        open
        onDismiss={() => undefined}
        onStartEmpty={onStartEmpty}
      />,
    );
    fireEvent.click(screen.getByTestId("onboarding-modal-start-empty"));
    expect(onStartEmpty).toHaveBeenCalledOnce();
  });

  it("dismisses on Escape", () => {
    const onDismiss = vi.fn();
    render(
      <OnboardingModal
        open
        onDismiss={onDismiss}
        onStartEmpty={() => undefined}
      />,
    );
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onDismiss).toHaveBeenCalledOnce();
  });

  it("dismisses when the backdrop is clicked, NOT when the dialog body is clicked", () => {
    const onDismiss = vi.fn();
    render(
      <OnboardingModal
        open
        onDismiss={onDismiss}
        onStartEmpty={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("onboarding-modal"));
    expect(onDismiss).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId("onboarding-modal-backdrop"));
    expect(onDismiss).toHaveBeenCalledOnce();
  });

  it("persists the dismissed flag via writeOnboardingDismissed and reads it back", () => {
    expect(readOnboardingDismissed()).toBe(false);
    writeOnboardingDismissed();
    expect(readOnboardingDismissed()).toBe(true);
    expect(
      window.localStorage.getItem(ONBOARDING_STORAGE_KEY),
    ).toBe("1");
  });
});
