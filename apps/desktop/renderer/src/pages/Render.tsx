import { useEffect, useState } from "react";
import { aec, RenderJob } from "../api/aec";
import { RenderQueue } from "../components/render/RenderQueue";
import {
  PresetSelector,
  RenderPresetKey,
} from "../components/render/PresetSelector";
import {
  CameraSelector,
  CameraTile,
} from "../components/render/CameraSelector";
import {
  RenderDoctor,
  DoctorSuggestion,
} from "../components/render/RenderDoctor";
import { RenderPreview } from "../components/render/RenderPreview";
import { BeforeAfterCompare } from "../components/render/BeforeAfterCompare";

const DEMO_CAMERAS: CameraTile[] = [
  {
    id: "cam_living",
    name: "Living wide",
    preset: "wide_angle",
    thumbnailDataUri: null,
  },
  {
    id: "cam_kitchen",
    name: "Kitchen close",
    preset: "interior_close_up",
    thumbnailDataUri: null,
  },
];

export function Render() {
  const [jobs, setJobs] = useState<RenderJob[]>([]);
  const [preset, setPreset] = useState<RenderPresetKey>("standard");
  const [selectedCameras, setSelectedCameras] = useState<Set<string>>(
    new Set(),
  );
  const [doctorJobId, setDoctorJobId] = useState<string | null>(null);
  const [doctorSuggestions, setDoctorSuggestions] = useState<
    DoctorSuggestion[]
  >([]);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let alive = true;
    void aec.render.listJobs().then((rows) => {
      if (alive) setJobs(rows as RenderJob[]);
    });
    return () => {
      alive = false;
    };
  }, []);

  const toggleCamera = (id: string) => {
    setSelectedCameras((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const enqueueAll = async () => {
    if (selectedCameras.size === 0) return;
    setBusy(true);
    try {
      const newJobs: RenderJob[] = [];
      for (const cameraId of selectedCameras) {
        const result = (await aec.render.enqueueRender({
          cameraId,
          preset,
        })) as { jobId: string };
        newJobs.push({
          jobId: result.jobId,
          status: "queued",
          preset,
          progress: 0,
        });
      }
      setJobs((prev) => [...newJobs, ...prev]);
    } finally {
      setBusy(false);
    }
  };

  const onCancel = (jobId: string) => {
    setJobs((prev) =>
      prev.map((j) =>
        j.jobId === jobId ? { ...j, status: "cancelled" } : j,
      ),
    );
  };

  return (
    <div className="render-layout" data-testid="render-mode">
      <header className="render-header">
        <h1>Render</h1>
        <button
          type="button"
          disabled={selectedCameras.size === 0 || busy}
          onClick={enqueueAll}
          data-testid="render-enqueue-all"
        >
          {busy
            ? "Queueing…"
            : `Queue ${selectedCameras.size} render${selectedCameras.size === 1 ? "" : "s"}`}
        </button>
      </header>
      <div className="render-grid">
        <aside className="render-sidebar">
          <PresetSelector active={preset} onChange={setPreset} />
          <CameraSelector
            cameras={DEMO_CAMERAS}
            selected={selectedCameras}
            onToggle={toggleCamera}
          />
        </aside>
        <main className="render-main">
          <RenderPreview imageDataUri={null} caption="Latest preview" />
          <BeforeAfterCompare before={null} after={null} />
        </main>
        <aside className="render-side-r">
          <RenderQueue jobs={jobs} onCancel={onCancel} />
          <RenderDoctor
            jobId={doctorJobId ?? jobs[0]?.jobId ?? null}
            suggestions={doctorSuggestions}
            onSuggestions={setDoctorSuggestions}
          />
          <div className="render-doctor__picker">
            <label htmlFor="doctor-job-id">Diagnose job:</label>
            <select
              id="doctor-job-id"
              value={doctorJobId ?? ""}
              onChange={(e) => setDoctorJobId(e.target.value || null)}
              data-testid="render-doctor-job-picker"
            >
              <option value="">(latest)</option>
              {jobs.map((j) => (
                <option key={j.jobId} value={j.jobId}>
                  {j.jobId}
                </option>
              ))}
            </select>
          </div>
        </aside>
      </div>
    </div>
  );
}
