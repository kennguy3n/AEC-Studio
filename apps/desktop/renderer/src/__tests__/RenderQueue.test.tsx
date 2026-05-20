import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { RenderQueue } from "../components/render/RenderQueue";
import type { RenderJob } from "../api/aec";

const JOBS: RenderJob[] = [
  { jobId: "job_001", status: "queued", preset: "standard", progress: 0 },
  { jobId: "job_002", status: "running", preset: "high", progress: 45 },
  { jobId: "job_003", status: "completed", preset: "studio", progress: 100 },
];

describe("RenderQueue", () => {
  it("shows the empty state when no jobs", () => {
    render(<RenderQueue jobs={[]} onCancel={() => undefined} />);
    expect(screen.getByTestId("render-queue").textContent).toContain(
      "No jobs queued",
    );
  });

  it("renders one row per job with progress", () => {
    render(<RenderQueue jobs={JOBS} onCancel={() => undefined} />);
    expect(screen.getByTestId("render-job-job_001")).toBeInTheDocument();
    expect(screen.getByTestId("render-job-job_002")).toBeInTheDocument();
    expect(
      (screen.getByTestId("render-job-progress-job_002") as HTMLProgressElement).value,
    ).toBe(45);
  });

  it("disables Cancel on terminal-state jobs and triggers IPC for in-flight ones", async () => {
    const onCancel = vi.fn();
    render(<RenderQueue jobs={JOBS} onCancel={onCancel} />);
    expect(
      (screen.getByTestId("render-job-cancel-job_003") as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    fireEvent.click(screen.getByTestId("render-job-cancel-job_002"));
    await waitFor(() =>
      expect(onCancel).toHaveBeenCalledWith("job_002"),
    );
  });
});
