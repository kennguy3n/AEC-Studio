import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import {
  RenderHistory,
  RenderHistoryEntry,
} from "../components/render/RenderHistory";

const ENTRIES: RenderHistoryEntry[] = [
  {
    jobId: "job_a",
    presetId: "standard",
    cameraName: "Living",
    completedAt: "2026-05-19T10:00:00Z",
    outputPath: "/renders/a.png",
    imageHash: "aaa",
    durationMs: 60_000,
  },
  {
    jobId: "job_b",
    presetId: "high",
    cameraName: "Kitchen",
    completedAt: "2026-05-19T12:00:00Z",
    outputPath: "/renders/b.png",
    imageHash: "bbb",
    durationMs: 240_000,
  },
];

describe("RenderHistory", () => {
  it("shows an empty state when no entries", () => {
    render(
      <RenderHistory
        entries={[]}
        selectedA={null}
        selectedB={null}
        onSelectA={() => undefined}
        onSelectB={() => undefined}
      />,
    );
    expect(screen.getByText("No completed renders yet.")).toBeInTheDocument();
  });

  it("renders newest entries first", () => {
    render(
      <RenderHistory
        entries={ENTRIES}
        selectedA={null}
        selectedB={null}
        onSelectA={() => undefined}
        onSelectB={() => undefined}
      />,
    );
    const rows = screen.getAllByTestId(/render-history-row-/);
    expect(rows[0].getAttribute("data-testid")).toBe(
      "render-history-row-job_b",
    );
    expect(rows[1].getAttribute("data-testid")).toBe(
      "render-history-row-job_a",
    );
  });

  it("toggles A selection via the pick button", () => {
    const onSelectA = vi.fn();
    render(
      <RenderHistory
        entries={ENTRIES}
        selectedA={null}
        selectedB={null}
        onSelectA={onSelectA}
        onSelectB={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("render-history-pick-a-job_a"));
    expect(onSelectA).toHaveBeenCalledWith("job_a");
  });

  it("clears A when the selected row's A button is clicked again", () => {
    const onSelectA = vi.fn();
    render(
      <RenderHistory
        entries={ENTRIES}
        selectedA="job_b"
        selectedB={null}
        onSelectA={onSelectA}
        onSelectB={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("render-history-pick-a-job_b"));
    expect(onSelectA).toHaveBeenCalledWith(null);
  });

  it("formats duration in m+s when >= 60s", () => {
    render(
      <RenderHistory
        entries={ENTRIES}
        selectedA={null}
        selectedB={null}
        onSelectA={() => undefined}
        onSelectB={() => undefined}
      />,
    );
    // job_b: 240_000ms = 4m00s
    const rowB = screen.getByTestId("render-history-row-job_b");
    expect(rowB.textContent).toContain("4m00s");
  });
});
