import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { BatchRenderModal } from "../components/render/BatchRenderModal";

const CAMERAS = [
  { id: "ent_a", name: "Living" },
  { id: "ent_b", name: "Kitchen" },
];

describe("BatchRenderModal", () => {
  it("returns null when closed", () => {
    const { container } = render(
      <BatchRenderModal
        open={false}
        cameras={CAMERAS}
        defaultPresetId="standard"
        onCancel={() => undefined}
        onSubmit={() => undefined}
      />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("defaults to all cameras × default preset selected", () => {
    render(
      <BatchRenderModal
        open
        cameras={CAMERAS}
        defaultPresetId="standard"
        onCancel={() => undefined}
        onSubmit={() => undefined}
      />,
    );
    expect(
      (screen.getByTestId("batch-render-camera-ent_a") as HTMLInputElement)
        .checked,
    ).toBe(true);
    expect(
      (screen.getByTestId("batch-render-camera-ent_b") as HTMLInputElement)
        .checked,
    ).toBe(true);
    expect(
      (screen.getByTestId("batch-render-preset-standard") as HTMLInputElement)
        .checked,
    ).toBe(true);
    // Two cameras × one preset = two jobs.
    expect(screen.getByText(/2 jobs will be queued/)).toBeInTheDocument();
  });

  it("submits the camera × preset matrix", async () => {
    const onSubmit = vi.fn().mockResolvedValue(undefined);
    render(
      <BatchRenderModal
        open
        cameras={CAMERAS}
        defaultPresetId="standard"
        onCancel={() => undefined}
        onSubmit={onSubmit}
      />,
    );
    fireEvent.click(screen.getByTestId("batch-render-preset-high"));
    fireEvent.click(screen.getByTestId("batch-render-submit"));
    await waitFor(() => expect(onSubmit).toHaveBeenCalled());
    const call = onSubmit.mock.calls[0][0];
    expect(call.cameraIds.sort()).toEqual(["ent_a", "ent_b"]);
    expect(call.presetIds.sort()).toEqual(["high", "standard"]);
  });

  it("disables submit when no cameras selected", () => {
    render(
      <BatchRenderModal
        open
        cameras={CAMERAS}
        defaultPresetId="standard"
        onCancel={() => undefined}
        onSubmit={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("batch-render-camera-ent_a"));
    fireEvent.click(screen.getByTestId("batch-render-camera-ent_b"));
    expect(
      (screen.getByTestId("batch-render-submit") as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });

  it("calls onCancel when Cancel is clicked", () => {
    const onCancel = vi.fn();
    render(
      <BatchRenderModal
        open
        cameras={CAMERAS}
        defaultPresetId="standard"
        onCancel={onCancel}
        onSubmit={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("batch-render-cancel"));
    expect(onCancel).toHaveBeenCalled();
  });

  it("shows an empty state when no cameras are available", () => {
    render(
      <BatchRenderModal
        open
        cameras={[]}
        defaultPresetId="standard"
        onCancel={() => undefined}
        onSubmit={() => undefined}
      />,
    );
    expect(
      screen.getByTestId("batch-render-cameras-empty"),
    ).toBeInTheDocument();
  });
});
