import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { LayoutSuggestionsPanel } from "../components/design/LayoutSuggestionsPanel";
import type { LayoutSuggestionProposal } from "../components/design/LayoutSuggestionsPanel";

const SAMPLE: LayoutSuggestionProposal[] = [
  {
    assetId: "ast:sofa",
    targetEntity: null,
    positionMm: [1200, 800, 0],
    rotationDeg: 90,
  },
  {
    assetId: null,
    targetEntity: "ent_existing_chair",
    positionMm: [2400, 1600, 0],
    rotationDeg: -45,
  },
];

describe("LayoutSuggestionsPanel", () => {
  it("disables the Suggest button when no room is selected", () => {
    render(
      <LayoutSuggestionsPanel
        roomAnchor={null}
        proposals={null}
        onSuggest={async () => []}
        onApplyProposal={() => undefined}
      />,
    );
    expect(
      (screen.getByTestId("layout-suggestions-run") as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });

  it("calls onSuggest with the active room and renders returned proposals", async () => {
    const onSuggest = vi.fn().mockResolvedValue([]);
    const { rerender } = render(
      <LayoutSuggestionsPanel
        roomAnchor="ent_living"
        proposals={null}
        onSuggest={onSuggest}
        onApplyProposal={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("layout-suggestions-run"));
    await waitFor(() =>
      expect(onSuggest).toHaveBeenCalledWith("ent_living"),
    );
    // Simulate parent setting proposals after the suggestion completes.
    rerender(
      <LayoutSuggestionsPanel
        roomAnchor="ent_living"
        proposals={SAMPLE}
        onSuggest={onSuggest}
        onApplyProposal={() => undefined}
      />,
    );
    expect(screen.getByTestId("layout-suggestions-list")).toBeInTheDocument();
    expect(screen.getByTestId("layout-suggestion-row-0").textContent).toContain(
      "Place ast:sofa",
    );
    expect(screen.getByTestId("layout-suggestion-row-1").textContent).toContain(
      "Move ent_existing_chair",
    );
  });

  it("emits onApplyProposal with the clicked row", () => {
    const onApply = vi.fn();
    render(
      <LayoutSuggestionsPanel
        roomAnchor="ent_living"
        proposals={SAMPLE}
        onSuggest={async () => SAMPLE}
        onApplyProposal={onApply}
      />,
    );
    fireEvent.click(screen.getByTestId("layout-suggestion-apply-0"));
    expect(onApply).toHaveBeenCalledWith(SAMPLE[0]);
    fireEvent.click(screen.getByTestId("layout-suggestion-apply-1"));
    expect(onApply).toHaveBeenCalledWith(SAMPLE[1]);
  });

  it("renders an empty state when proposals is an empty array", () => {
    render(
      <LayoutSuggestionsPanel
        roomAnchor="ent_living"
        proposals={[]}
        onSuggest={async () => []}
        onApplyProposal={() => undefined}
      />,
    );
    expect(screen.getByTestId("layout-suggestions-empty")).toBeInTheDocument();
  });

  it("surfaces errors from onSuggest as a role=alert message", async () => {
    const onSuggest = vi.fn().mockRejectedValue(new Error("sidecar offline"));
    render(
      <LayoutSuggestionsPanel
        roomAnchor="ent_living"
        proposals={null}
        onSuggest={onSuggest}
        onApplyProposal={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("layout-suggestions-run"));
    await waitFor(() => {
      expect(screen.getByRole("alert").textContent).toContain(
        "sidecar offline",
      );
    });
  });
});
