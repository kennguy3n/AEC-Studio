/**
 * Smoke tests for the Deliver mode page.
 *
 * The page wires four components (toolbar, pack composer, export
 * targets, revision manager) against the in-process aec.deliver
 * fixture. These tests exercise the full create → list → compare flow
 * and the pack-build flow.
 */

import { describe, it, expect } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

import { Deliver } from "../pages/Deliver";

describe("<Deliver />", () => {
  it("renders pack composer, export targets, and revision manager", () => {
    render(<Deliver />);
    expect(screen.getByTestId("deliver-mode")).toBeInTheDocument();
    expect(screen.getByTestId("pack-composer")).toBeInTheDocument();
    expect(screen.getByTestId("export-target-list")).toBeInTheDocument();
    expect(screen.getByTestId("revision-manager")).toBeInTheDocument();
    expect(screen.getByTestId("deliver-toolbar")).toBeInTheDocument();
  });

  it("seeds default deliverables for concept pack on first mount", () => {
    render(<Deliver />);
    const rendersBox = screen
      .getByTestId("pack-deliverable-renders")
      .querySelector("input") as HTMLInputElement;
    const ifcBox = screen
      .getByTestId("pack-deliverable-ifc")
      .querySelector("input") as HTMLInputElement;
    // concept pack: renders on, IFC off+disabled (not applicable).
    expect(rendersBox.checked).toBe(true);
    expect(ifcBox.disabled).toBe(true);
  });

  it("switches deliverables when pack kind changes", () => {
    render(<Deliver />);
    const contractorRadio = screen
      .getByTestId("pack-kind-contractor")
      .querySelector("input") as HTMLInputElement;
    fireEvent.click(contractorRadio);
    const ifcBox = screen
      .getByTestId("pack-deliverable-ifc")
      .querySelector("input") as HTMLInputElement;
    expect(ifcBox.disabled).toBe(false);
    expect(ifcBox.checked).toBe(true);
  });

  it("creates a revision, then lists it", async () => {
    render(<Deliver />);
    fireEvent.change(screen.getByTestId("revision-tag-input"), {
      target: { value: "v1" },
    });
    fireEvent.change(screen.getByTestId("revision-description-input"), {
      target: { value: "first cut" },
    });
    fireEvent.click(screen.getByTestId("revision-create-button"));
    await waitFor(() => {
      expect(screen.queryByTestId("revision-empty")).not.toBeInTheDocument();
    });
    // The revision entry id is dynamic; look for the strong tag text.
    expect(screen.getByText("v1")).toBeInTheDocument();
    expect(screen.getByText("first cut")).toBeInTheDocument();
  });

  it("disables compare until two distinct revisions are picked", async () => {
    render(<Deliver />);
    const compareBtn = screen.getByTestId(
      "revision-compare-button",
    ) as HTMLButtonElement;
    expect(compareBtn.disabled).toBe(true);

    // Create two revisions.
    for (const tag of ["v1", "v2"]) {
      fireEvent.change(screen.getByTestId("revision-tag-input"), {
        target: { value: tag },
      });
      fireEvent.click(screen.getByTestId("revision-create-button"));
      await waitFor(() => {
        expect(screen.getByText(tag)).toBeInTheDocument();
      });
    }

    // Pick base = v1, head = v2 by clicking the per-row buttons.
    const baseButtons = screen.getAllByText(/Set as base/);
    fireEvent.click(baseButtons[0]);
    const headButtons = screen.getAllByText(/Set as head/);
    fireEvent.click(headButtons[headButtons.length - 1]);

    await waitFor(() => {
      expect(
        (screen.getByTestId("revision-compare-button") as HTMLButtonElement)
          .disabled,
      ).toBe(false);
    });

    fireEvent.click(screen.getByTestId("revision-compare-button"));
    await waitFor(() => {
      expect(screen.getByTestId("revision-diff-summary")).toBeInTheDocument();
    });
  });

  it("builds a pack and renders the resulting file list", async () => {
    render(<Deliver />);
    fireEvent.click(screen.getByTestId("pack-build"));
    await waitFor(() => {
      expect(screen.getByTestId("deliver-export-result")).toBeInTheDocument();
    });
    // Concept pack ships a manifest in our fixture.
    expect(screen.getByTestId("pack-file-manifest.json")).toBeInTheDocument();
  });
});
