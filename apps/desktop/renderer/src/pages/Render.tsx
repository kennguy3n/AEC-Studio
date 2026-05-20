import { useCallback, useEffect, useState } from "react";
import { aec, RenderJob, RuntimeStatus } from "../api/aec";
import { RenderQueue } from "../components/render/RenderQueue";
import {
  PresetSelector,
  RenderPresetKey,
  recommendedPresetFor,
} from "../components/render/PresetSelector";
import {
  CameraSelector,
  CameraTile,
} from "../components/render/CameraSelector";
import {
  LightingPresetSelector,
  LightingPresetId,
} from "../components/render/LightingPresetSelector";
import {
  RenderDoctor,
  DoctorSuggestion,
} from "../components/render/RenderDoctor";
import { RenderPreview } from "../components/render/RenderPreview";
import { BeforeAfterCompare } from "../components/render/BeforeAfterCompare";

function recommendedFor(tier: RuntimeStatus["tier"]): RenderPresetKey {
  return recommendedPresetFor(tier);
}

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
  const [lighting, setLighting] = useState<LightingPresetId>("daylight");
  const [tier, setTier] = useState<RuntimeStatus["tier"] | null>(null);
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
    void aec.runtime.status().then((status) => {
      if (!alive) return;
      const rs = status as RuntimeStatus;
      setTier(rs.tier);
      // Pre-select the recommended preset for the detected tier so the
      // first render matches the machine's capabilities. Users can
      // still pick anything they like afterwards.
      const recommended = recommendedFor(rs.tier);
      if (recommended) setPreset(recommended);
    });
    return () => {
      alive = false;
    };
  }, []);

  const changePreset = useCallback((next: RenderPresetKey) => {
    setPreset(next);
    // Persist the choice on the backend so other surfaces (queue UI,
    // diagnostics) see the active preset. The bridge stub returns
    // `{ ok: true }` in dev; the real backend persists.
    void aec.render.applyPreset({ preset: next });
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
          <PresetSelector
            active={preset}
            onChange={changePreset}
            tier={tier ?? undefined}
          />
          <LightingPresetSelector active={lighting} onChange={setLighting} />
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
