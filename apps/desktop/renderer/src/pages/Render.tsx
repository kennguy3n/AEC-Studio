import { useEffect, useState } from "react";
import { aec, RenderJob } from "../api/aec";

export function Render() {
  const [jobs, setJobs] = useState<RenderJob[]>([]);

  useEffect(() => {
    let alive = true;
    void aec.render.listJobs().then((rows) => {
      if (alive) setJobs(rows as RenderJob[]);
    });
    return () => {
      alive = false;
    };
  }, []);

  return (
    <div data-testid="render-mode">
      <h1>Render</h1>
      <p>EEVEE previews, Cycles final renders, batch queue, render doctor.</p>
      <section style={{ marginTop: 24 }}>
        <h2 className="home__section-title">Queue</h2>
        {jobs.length === 0 ? (
          <div className="card">No jobs queued.</div>
        ) : (
          <ul>
            {jobs.map((j) => (
              <li key={j.jobId} className="card" style={{ marginBottom: 8 }}>
                <strong>{j.jobId}</strong> · {j.preset} · {j.status} ({j.progress}%)
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
