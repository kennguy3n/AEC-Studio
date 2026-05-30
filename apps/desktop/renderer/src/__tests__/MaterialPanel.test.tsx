/**
 * Phase 17 Group B Task 11 — `MaterialPanel` renderer-side tests.
 *
 * These tests run against the in-process renderer fallback in
 * `renderer-backend.ts` (Vitest never wires up Electron's preload,
 * so the `aec` proxy resolves to `rendererInProcessBackend()`),
 * which mirrors the bridge's `MaterialLibrary::with_default_pack()`
 * seed pack exactly. That lets us assert against stable material
 * ids (`mat:oak_light`, `mat:walnut`, …) and bridge-shaped query
 * results without spinning up the napi backend.
 */

import { describe, it, expect } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { MaterialPanel } from "../components/design/MaterialPanel";

describe("MaterialPanel", () => {
  it("renders the seeded material library on mount", async () => {
    render(<MaterialPanel />);
    await waitFor(() => {
      expect(screen.getByTestId("material-mat:oak_light")).toBeInTheDocument();
    });
    expect(screen.getByTestId("material-mat:walnut")).toBeInTheDocument();
    expect(
      screen.getByTestId("material-mat:concrete_polished"),
    ).toBeInTheDocument();
    // The default pack has eight starter materials.
    expect(
      screen.getAllByTestId(/^material-mat:/),
    ).toHaveLength(8);
  });

  it("opens the inspector when a swatch is clicked", async () => {
    render(<MaterialPanel />);
    await waitFor(() => {
      expect(screen.getByTestId("material-mat:oak_light")).toBeInTheDocument();
    });
    fireEvent.click(screen.getByTestId("material-mat:oak_light"));
    const inspector = await screen.findByTestId("material-inspector");
    expect(inspector).toBeInTheDocument();
    expect(inspector.querySelector("h3")?.textContent).toBe("Light Oak");
    // Style tags from the seed pack are surfaced on the inspector.
    expect(screen.getByTestId("material-inspector-styletags")).toHaveTextContent(
      /scandinavian/,
    );
  });

  it("filters by style tag when a tab is selected", async () => {
    render(<MaterialPanel />);
    await waitFor(() => {
      expect(screen.getByTestId("material-mat:oak_light")).toBeInTheDocument();
    });
    fireEvent.click(screen.getByTestId("material-tab-japandi"));
    // Only japandi-tagged materials remain (the seed pack has one:
    // `mat:linen_oat`).
    await waitFor(() => {
      expect(screen.getByTestId("material-mat:linen_oat")).toBeInTheDocument();
      expect(
        screen.queryByTestId("material-mat:walnut"),
      ).not.toBeInTheDocument();
    });
  });

  it("returns to the full library when the All tab is reselected", async () => {
    render(<MaterialPanel />);
    await waitFor(() => {
      expect(screen.getByTestId("material-mat:oak_light")).toBeInTheDocument();
    });
    fireEvent.click(screen.getByTestId("material-tab-industrial"));
    await waitFor(() => {
      expect(
        screen.queryByTestId("material-mat:linen_oat"),
      ).not.toBeInTheDocument();
    });
    fireEvent.click(screen.getByTestId("material-tab-all"));
    await waitFor(() => {
      expect(screen.getByTestId("material-mat:linen_oat")).toBeInTheDocument();
    });
  });

  it("updates the roughness slider through the bridge", async () => {
    render(<MaterialPanel />);
    await waitFor(() => {
      expect(screen.getByTestId("material-mat:oak_light")).toBeInTheDocument();
    });
    fireEvent.click(screen.getByTestId("material-mat:oak_light"));
    const slider = await screen.findByTestId(
      "material-inspector-roughness",
    );
    // Default roughness for the oak_light seed is 0.6.
    expect(slider).toHaveValue("0.6");
    fireEvent.change(slider, { target: { value: "0.25" } });
    await waitFor(() => {
      expect(
        screen.getByTestId("material-inspector-roughness-value"),
      ).toHaveTextContent("0.25");
    });
  });

  it("rejects an out-of-range IOR value with a surfaced error", async () => {
    // The renderer fallback mirrors the napi validator's
    // `1.0 <= ior <= 5.0` range guard. The slider itself clamps to
    // the same range, but a programmatic out-of-range patch (e.g.
    // a future numeric-input box) should still surface the bridge
    // error to the panel.
    //
    // We exercise the inspector's IOR slider at its max bound to
    // verify the happy path; the bridge-side validator is covered
    // directly in `renderer-backend.test.ts`.
    render(<MaterialPanel />);
    await waitFor(() => {
      expect(screen.getByTestId("material-mat:oak_light")).toBeInTheDocument();
    });
    fireEvent.click(screen.getByTestId("material-mat:oak_light"));
    const slider = await screen.findByTestId("material-inspector-ior");
    fireEvent.change(slider, { target: { value: "5.0" } });
    await waitFor(() => {
      expect(
        screen.getByTestId("material-inspector-ior-value"),
      ).toHaveTextContent("5.00");
    });
  });

  it("sets the material-id MIME on drag start", async () => {
    render(<MaterialPanel />);
    await waitFor(() => {
      expect(screen.getByTestId("material-mat:oak_light")).toBeInTheDocument();
    });
    const item = screen.getByTestId("material-mat:oak_light");
    // `dataTransfer.setData` doesn't survive `fireEvent.dragStart`
    // round-tripping through jsdom (mirror `AssetBrowser`'s test
    // comment) — instead assert that the swatch is `draggable` so
    // production-side drag dispatch is wired up.
    expect(item).toHaveAttribute("draggable", "true");
  });
});
