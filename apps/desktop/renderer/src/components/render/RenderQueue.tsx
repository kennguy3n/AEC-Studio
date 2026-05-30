import { useEffect, useState } from "react";
import { aec, RenderJob } from "../../api/aec";

interface Props {
  jobs: RenderJob[];
  onCancel: (jobId: string) => void;
  /**
   * Optional "now" override so tests can pin the ETA calculation
   * against a deterministic wall clock. Defaults to `Date.now()` at
   * render time. The component reads it via a function so the
   * default closes over the current tick rather than the module
   * load tick (the latter would freeze every preview render to the
   * page-load time and produce nonsensical ETAs in long-lived
   * sessions).
   */
  now?: () => number;
}

export function RenderQueue({ jobs, onCancel, now }: Props) {
  const [busyId, setBusyId] = useState<string | null>(null);
  // Tick once per second while there's at least one running job so the
  // per-row + batch ETA labels visibly count down between bridge
  // polls. Without this, `nowMs` is only refreshed when the parent's
  // `setJobs([…])` re-renders us — typically every few seconds at the
  // poll cadence — making the ETA appear frozen for tests / users
  // who expect a smooth countdown. The tick is suspended once every
  // job has reached a terminal status to avoid unnecessary renders.
  // The `now` test override skips the tick entirely so unit tests stay
  // deterministic against a pinned wall-clock.
  const [tickMs, setTickMs] = useState(() => Date.now());
  const hasRunning = jobs.some((j) => j.status === "running");
  useEffect(() => {
    if (now !== undefined) return;
    if (!hasRunning) return;
    const id = window.setInterval(() => setTickMs(Date.now()), 1000);
    return () => window.clearInterval(id);
  }, [now, hasRunning]);
  const nowMs = now !== undefined ? now() : tickMs;

  const cancel = async (jobId: string) => {
    setBusyId(jobId);
    try {
      await aec.render.cancelJob(jobId);
      onCancel(jobId);
    } finally {
      setBusyId(null);
    }
  };

  if (jobs.length === 0) {
    return (
      <section
        className="render-queue render-queue--empty"
        data-testid="render-queue"
      >
        <p>No jobs queued.</p>
      </section>
    );
  }

  // Phase 17 Group C Task 18 — batch-level ETA across all currently
  // running jobs, so the user sees an aggregate "remaining time"
  // header above the per-row table. Computed as the *maximum* of
  // the individual ETAs because in practice the queue runs jobs
  // serially (one path-tracer at a time on the GPU); the batch
  // can't finish before the slowest current job does. When no jobs
  // are running, or no running job has enough progress to estimate,
  // the batch ETA is hidden.
  const runningEtas = jobs
    .filter((j) => j.status === "running")
    .map((j) => computeEtaMs(j, nowMs))
    .filter((ms): ms is number => ms !== null);
  const batchEtaMs =
    runningEtas.length > 0 ? Math.max(...runningEtas) : null;

  return (
    <section
      className="render-queue"
      aria-label="Render queue"
      data-testid="render-queue"
    >
      {batchEtaMs !== null && (
        <header
          className="render-queue__batch-eta"
          data-testid="render-queue-batch-eta"
        >
          Batch ETA: {formatDuration(batchEtaMs)}
        </header>
      )}
      <table>
        <thead>
          <tr>
            <th scope="col">Job</th>
            <th scope="col">Preset</th>
            <th scope="col">Status</th>
            <th scope="col">Progress</th>
            <th scope="col">ETA</th>
            <th scope="col" aria-label="actions" />
          </tr>
        </thead>
        <tbody>
          {jobs.map((j) => (
            <tr key={j.jobId} data-testid={`render-job-${j.jobId}`}>
              <th scope="row">{j.jobId}</th>
              <td>{j.preset}</td>
              <td>{j.status}</td>
              <td>
                <progress
                  value={j.progress}
                  max={100}
                  data-testid={`render-job-progress-${j.jobId}`}
                />{" "}
                {Math.round(j.progress)}%
              </td>
              <td data-testid={`render-job-eta-${j.jobId}`}>
                {formatRowEta(j, nowMs)}
              </td>
              <td>
                <button
                  type="button"
                  data-testid={`render-job-cancel-${j.jobId}`}
                  disabled={
                    busyId === j.jobId ||
                    j.status === "completed" ||
                    j.status === "failed" ||
                    j.status === "cancelled"
                  }
                  onClick={() => cancel(j.jobId)}
                >
                  Cancel
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </section>
  );
}

/**
 * Phase 17 Group C Task 18 — real per-job ETA.
 *
 * Estimates remaining time from elapsed wall-clock time and the
 * job's progress fraction:
 *
 *   `remainingMs = elapsedMs · (1 - progress) / progress`
 *
 * Returns `null` when ETA is undefined (the job hasn't started yet,
 * it's already terminal, the `startedAt` timestamp didn't survive
 * the bridge round-trip, or progress is too low to extrapolate
 * reliably — see the `progress > 0.05` floor below).
 */
function computeEtaMs(j: RenderJob, nowMs: number): number | null {
  if (j.status !== "running") return null;
  if (!j.startedAt) return null;
  const startedMs = Date.parse(j.startedAt);
  if (Number.isNaN(startedMs)) return null;
  // `RenderJob.progress` is normalised to a percentage in `[0, 100]`
  // at the bridge boundary (see `apps/desktop/electron/bridge.ts`
  // `renderListJobs`), so divide here to recover the fraction the
  // arithmetic below expects. Earlier revisions used a `> 1`
  // discriminator to accept either `0..1` or `0..100` callers; that
  // discriminator was ambiguous at `progress === 1` (1 % vs. 100 %)
  // — normalising at the boundary removes the ambiguity entirely.
  const fraction = j.progress / 100;
  // Below 5% the elapsed-time extrapolation is dominated by
  // bridge / preset warm-up overhead (BVH build, texture upload,
  // light-tree assembly) and produces wildly pessimistic estimates
  // — the user sees "~14m 32s remaining" for a render that will
  // actually finish in 90s. Surface a "calculating…" label until
  // the path tracer has accumulated enough samples for the linear
  // extrapolation to be meaningful.
  if (fraction <= 0.05) return null;
  if (fraction >= 1.0) return 0;
  const elapsed = nowMs - startedMs;
  if (elapsed <= 0) return null;
  const remaining = (elapsed * (1 - fraction)) / fraction;
  if (!Number.isFinite(remaining) || remaining < 0) return null;
  return remaining;
}

function formatRowEta(j: RenderJob, nowMs: number): string {
  if (j.status === "completed") return "—";
  if (j.status === "failed" || j.status === "cancelled") return "—";
  if (j.status === "queued") return "—";
  const remaining = computeEtaMs(j, nowMs);
  if (remaining === null) {
    // `running` job below the 5% confidence floor — show
    // calculating… so the row still surfaces ETA semantics rather
    // than the previous placeholder dash that looked like "no ETA
    // ever".
    return "calculating…";
  }
  return formatDuration(remaining);
}

function formatDuration(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remSec = seconds % 60;
  if (minutes < 60) return `${minutes}m ${remSec}s`;
  const hours = Math.floor(minutes / 60);
  const remMin = minutes % 60;
  return `${hours}h ${remMin}m`;
}
