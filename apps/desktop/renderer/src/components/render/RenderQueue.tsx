import { useState } from "react";
import { aec, RenderJob } from "../../api/aec";

interface Props {
  jobs: RenderJob[];
  onCancel: (jobId: string) => void;
}

export function RenderQueue({ jobs, onCancel }: Props) {
  const [busyId, setBusyId] = useState<string | null>(null);

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
  return (
    <section
      className="render-queue"
      aria-label="Render queue"
      data-testid="render-queue"
    >
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
                {j.progress}%
              </td>
              <td>{formatEta(j)}</td>
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

function formatEta(j: RenderJob): string {
  if (j.status !== "running") return "—";
  if (j.progress <= 0 || j.progress >= 100) return "—";
  // ETA is a rough placeholder: assume 10s per remaining percent point.
  const remaining = (100 - j.progress) * 10;
  if (remaining < 60) return `${Math.round(remaining)}s`;
  const min = Math.floor(remaining / 60);
  return `${min}m${Math.round(remaining - min * 60)}s`;
}
