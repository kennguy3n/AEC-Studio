import { useCallback, useEffect, useRef, useState } from "react";
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
import { useActiveProject } from "../hooks/useActiveProject";
import { useToast } from "../hooks/useToast";

function recommendedFor(tier: RuntimeStatus["tier"]): RenderPresetKey {
  return recommendedPresetFor(tier);
}

// Empty list is the *only* legal starting state for the camera
// selector. A previous incarnation seeded `FALLBACK_CAMERAS` with two
// fake demo entries (`cam_living`, `cam_kitchen`) so the UI looked
// populated on a brand-new project. Devin Review flagged the
// resulting UX hazard: the fallback persisted whenever `listGraph`
// returned zero camera entities, so the user could `toggleCamera` a
// fake ID and click "Queue renders" — the per-camera
// `aec.render.enqueueRender({ cameraId: "cam_living", ... })` call
// would either fail (native bridge rejecting the unknown entity) or
// silently enqueue an orphaned job that no diagnose / cancel call
// could subsequently address. The correct contract is: cameras list
// is exactly what the bridge reports for the active project, and
// `CameraSelector` renders its built-in empty-state when the list is
// empty (see `CameraSelector.tsx:14-27`).
const EMPTY_CAMERAS: CameraTile[] = [];

export function Render() {
  const { project } = useActiveProject();
  const { addToast } = useToast();
  const [jobs, setJobs] = useState<RenderJob[]>([]);
  const [preset, setPreset] = useState<RenderPresetKey>("standard");
  const [lighting, setLighting] = useState<LightingPresetId>("daylight");
  const [tier, setTier] = useState<RuntimeStatus["tier"] | null>(null);
  const userChosePresetRef = useRef(false);
  const [cameras, setCameras] = useState<CameraTile[]>(EMPTY_CAMERAS);
  const [selectedCameras, setSelectedCameras] = useState<Set<string>>(
    new Set(),
  );
  const [doctorJobId, setDoctorJobId] = useState<string | null>(null);
  const [doctorSuggestions, setDoctorSuggestions] = useState<
    DoctorSuggestion[]
  >([]);
  const [busy, setBusy] = useState(false);

  // Mount-only: detect the hardware tier and pick the recommended
  // preset on the user's first visit to the page. The runtime tier
  // (CPU cores, GPU model, RAM) is static for the renderer process —
  // hot-plugging a GPU mid-session would require a full app restart
  // anyway — so re-fetching on every project transition would be
  // pure IPC waste (matches the `StatusBar.tsx:16-27` pattern for
  // the same `runtime.status()` call). The recommended-preset
  // application is also intentionally one-shot via
  // `userChosePresetRef`: once the user picks a preset, the tier
  // badge inside `PresetSelector` still advertises the recommended
  // value so they can switch back manually, but we never overwrite
  // their explicit choice on a re-render.
  useEffect(() => {
    let alive = true;
    void aec.runtime.status().then((status) => {
      if (!alive) return;
      const rs = status as RuntimeStatus;
      setTier(rs.tier);
      if (userChosePresetRef.current) return;
      const recommended = recommendedFor(rs.tier);
      if (recommended) setPreset(recommended);
    });
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    let alive = true;
    // Reset per-project state *synchronously* before any async load.
    // Without this, the `cameras` list and `selectedCameras` set carry
    // over from the previous project — the user would see project A's
    // cameras (or worse, project A's IDs in the selection set) after
    // opening project B. The selection set is always cleared because
    // camera IDs are not valid across projects: enqueueing a render
    // with a stale ID creates an orphaned job that the native backend
    // would reject. When the active project closes (`project?.path`
    // becomes nullish), the reset still runs so the queue / camera
    // grid don't strand the user looking at the previous project's
    // entities on the route-guard's home page.
    //
    // `jobs` is also reset synchronously even though the immediately-
    // following `listJobs()` (when a project is open) will repopulate
    // it. Without the synchronous reset there is a one-microtask
    // window after the project transition (between this effect
    // committing and the `listJobs()` promise resolving) where the
    // previous project's jobs would render in the queue. The flash is
    // brief but visible, and a stale entry whose job ID belongs to
    // the previous project would also fail any subsequent
    // `cancelJob` / `diagnose` calls that key off it. Mirrors the
    // `setCameras` / `setSelectedCameras` pattern so all per-project
    // queue state transitions together.
    setCameras(EMPTY_CAMERAS);
    setSelectedCameras(new Set());
    setJobs([]);
    // Gate every bridge fetch on a live project. Both `listJobs` and
    // `listGraph` are project-scoped queries: with no active project,
    // the native handlers have no DB to address and the fallback
    // backend short-circuits to `[]`. Issuing the calls anyway burns
    // one IPC round-trip per route transition for no observable
    // benefit, and the resulting `setJobs([])` triggers a redundant
    // commit that the synchronous reset above already covered.
    // Matches the `StatusBar.tsx:62-93` pattern, which gates
    // `project:save:status` polling on `projectPath !== null` for the
    // same reason.
    if (project?.path) {
      void aec.render
        .listJobs()
        .then((rows) => {
          if (alive) setJobs(rows as RenderJob[]);
        })
        .catch(() => {
          // Bridge failure (project closed mid-fetch, corrupt
          // render-jobs row, permission denied on the render-store
          // DB) — keep the empty list from the synchronous reset
          // above so the queue UI doesn't carry stale entries from
          // a previous successful fetch. Matches the `.catch()` on
          // `listGraph` below and the `StatusBar.tsx` /
          // `Deliver.tsx` polling-tick convention: silent fail,
          // empty/last-known UI, no toast (routine project
          // switches race the bridge's own teardown and would
          // otherwise spam transient-failure toasts). The bridge
          // layer logs the underlying error.
        });
      void aec.command
        .listGraph(project.path, "camera")
        .then((entities) => {
          if (!alive) return;
          if (Array.isArray(entities) && entities.length > 0) {
            const mapped: CameraTile[] = entities.map((e) => {
              const body = e.body as Record<string, unknown> | null;
              const bodyName =
                body !== null && typeof body === "object" && typeof body.name === "string"
                  ? body.name
                  : null;
              return {
                id: e.id,
                name: bodyName ?? `Camera ${e.id}`,
                preset: "standard",
                thumbnailDataUri: null,
              };
            });
            setCameras(mapped);
          }
          // entities is `[]` → cameras stays empty (synchronous reset
          // above); `CameraSelector` renders the empty-state UI.
        })
        .catch(() => {
          // Bridge failure (corrupt DB, permission denied) — keep the
          // empty list so the user sees the empty-state instead of
          // stale or fake entries. Logged at the bridge layer.
        });
    }
    return () => {
      alive = false;
    };
  }, [project?.path]);

  const changePreset = useCallback((next: RenderPresetKey) => {
    setPreset(next);
    userChosePresetRef.current = true;
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
    // Per-camera enqueue. Each `aec.render.enqueueRender` call is
    // its own bridge round-trip — a single bad camera (stale ID, an
    // orphan left over from a project the user navigated away from
    // before the camera-reset effect committed, or any bridge-side
    // validation failure such as a tier downgrade rejecting the
    // requested preset) must NOT silently abort the whole batch with
    // no user-visible feedback. Pre-Phase 13 the symmetric success
    // toast was added but the error path was left to bubble up as an
    // unhandled promise from the `onClick` handler — the user saw
    // the cameras that succeeded committed to the queue and the
    // rest disappear without explanation. Wrap each call so we
    // accumulate the partial result, then surface a success toast
    // for what landed AND an error toast for what failed (with the
    // failed camera IDs + bridge error in the message body so the
    // user can act on it). Any cameras that succeeded before the
    // first failure DO commit to the queue — they have valid job
    // IDs and the bridge already accepted them, so it would be
    // misleading to drop them from the UI. The user-actionable info
    // is which cameras didn't make it.
    const newJobs: RenderJob[] = [];
    const failures: { cameraId: string; message: string }[] = [];
    try {
      for (const cameraId of selectedCameras) {
        try {
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
        } catch (err) {
          failures.push({
            cameraId,
            message: err instanceof Error ? err.message : String(err),
          });
        }
      }
      if (newJobs.length > 0) {
        setJobs((prev) => [...newJobs, ...prev]);
        addToast(
          "success",
          `Queued ${newJobs.length} render${newJobs.length === 1 ? "" : "s"}`,
        );
      }
      if (failures.length > 0) {
        const detail = failures
          .map((f) => `${f.cameraId}: ${f.message}`)
          .join(", ");
        addToast(
          "error",
          `Failed to queue ${failures.length} render${failures.length === 1 ? "" : "s"} (${detail})`,
        );
      }
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
    void aec.render.cancelJob(jobId).catch(() => {
      // Best-effort cancel — the job may already be done.
    });
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
            cameras={cameras}
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
