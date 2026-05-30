import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { RenderQueue } from "../components/render/RenderQueue";
import type { RenderJob } from "../api/aec";

// Phase 17 Group C Task 18 — ETA tests use a pinned wall-clock so the
// elapsed-time arithmetic is deterministic. `T0` is a job's startedAt,
// `T_NOW` is the test's frozen "now".
const T0 = "2025-05-30T08:00:00.000Z";
const T_NOW = Date.parse("2025-05-30T08:01:00.000Z"); // 60s later

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

  // ---- Phase 17 Group C Task 18: per-row and batch ETA ----
  it("calculates per-row ETA from elapsed time + progress", () => {
    // 60s elapsed, 50% progress → 60s remaining.
    const job: RenderJob = {
      jobId: "job_eta",
      status: "running",
      preset: "standard",
      progress: 50,
      startedAt: T0,
    };
    render(
      <RenderQueue
        jobs={[job]}
        onCancel={() => undefined}
        now={() => T_NOW}
      />,
    );
    expect(
      screen.getByTestId("render-job-eta-job_eta").textContent,
    ).toBe("1m 0s");
  });

  it("surfaces 'calculating\u2026' when progress is below the 5% confidence floor", () => {
    const job: RenderJob = {
      jobId: "job_cold",
      status: "running",
      preset: "standard",
      progress: 2,
      startedAt: T0,
    };
    render(
      <RenderQueue
        jobs={[job]}
        onCancel={() => undefined}
        now={() => T_NOW}
      />,
    );
    expect(
      screen.getByTestId("render-job-eta-job_cold").textContent,
    ).toBe("calculating\u2026");
  });

  it("shows '\u2014' for terminal-state jobs (completed / failed / cancelled / queued)", () => {
    const jobs: RenderJob[] = [
      {
        jobId: "job_q",
        status: "queued",
        preset: "standard",
        progress: 0,
      },
      {
        jobId: "job_done",
        status: "completed",
        preset: "standard",
        progress: 100,
      },
    ];
    render(
      <RenderQueue jobs={jobs} onCancel={() => undefined} now={() => T_NOW} />,
    );
    expect(
      screen.getByTestId("render-job-eta-job_q").textContent,
    ).toBe("\u2014");
    expect(
      screen.getByTestId("render-job-eta-job_done").textContent,
    ).toBe("\u2014");
  });

  it("shows a batch-level ETA header reflecting the slowest running job", () => {
    const jobs: RenderJob[] = [
      {
        jobId: "fast",
        status: "running",
        preset: "standard",
        progress: 80,
        startedAt: T0, // 60s elapsed, 80% → 15s remaining
      },
      {
        jobId: "slow",
        status: "running",
        preset: "high",
        progress: 40,
        startedAt: T0, // 60s elapsed, 40% → 90s remaining
      },
    ];
    render(
      <RenderQueue jobs={jobs} onCancel={() => undefined} now={() => T_NOW} />,
    );
    // Batch ETA = max(15s, 90s) = 90s = 1m 30s
    expect(
      screen.getByTestId("render-queue-batch-eta").textContent,
    ).toContain("1m 30s");
  });

  it("hides the batch ETA header when no running jobs have a confident ETA", () => {
    // `progress: 3` is in 0–100 form — the canonical unit at the
    // bridge boundary (`apps/desktop/electron/bridge.ts`,
    // `renderListJobs`). 3 % sits below `computeEtaMs`'s 5 %
    // confidence floor, so every running job produces `null` and the
    // batch header is suppressed.
    render(
      <RenderQueue
        jobs={[
          {
            jobId: "cold",
            status: "running",
            preset: "standard",
            progress: 3,
            startedAt: T0,
          },
        ]}
        onCancel={() => undefined}
        now={() => T_NOW}
      />,
    );
    expect(
      screen.queryByTestId("render-queue-batch-eta"),
    ).not.toBeInTheDocument();
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
