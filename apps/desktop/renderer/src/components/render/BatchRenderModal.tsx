import { useEffect, useMemo, useState } from "react";
import { PRESET_IDS } from "./PresetSelector";

export interface BatchCameraOption {
  id: string;
  name: string;
}

interface Props {
  open: boolean;
  cameras: BatchCameraOption[];
  defaultPresetId: string;
  onCancel: () => void;
  onSubmit: (params: {
    cameraIds: string[];
    presetIds: string[];
  }) => Promise<void> | void;
}

/**
 * Modal that lets the user pick a subset of saved cameras and one or
 * more render presets, then submits a batch via
 * `aec.render.enqueueBatch`. The matrix mode (multiple presets selected)
 * mirrors the Rust `RenderQueue::submit_matrix` so the resulting job
 * count is `cameras.len() * presets.len()`.
 */
export function BatchRenderModal({
  open,
  cameras,
  defaultPresetId,
  onCancel,
  onSubmit,
}: Props) {
  const [selectedCameras, setSelectedCameras] = useState<Set<string>>(
    () => new Set(cameras.map((c) => c.id)),
  );
  const [selectedPresets, setSelectedPresets] = useState<Set<string>>(
    () => new Set([defaultPresetId]),
  );
  const [busy, setBusy] = useState(false);

  // The modal is rendered as `null` when `open === false` but never
  // unmounted by the parent, so re-opening with a different `cameras`
  // prop would otherwise show stale checkbox state from the previous
  // open. Re-seed the selection every time the modal opens or the
  // caller-supplied cameras change so the visible checkboxes always
  // match the current camera list.
  useEffect(() => {
    if (!open) return;
    setSelectedCameras(new Set(cameras.map((c) => c.id)));
    setSelectedPresets(new Set([defaultPresetId]));
  }, [open, cameras, defaultPresetId]);

  const jobCount = useMemo(
    () => selectedCameras.size * selectedPresets.size,
    [selectedCameras, selectedPresets],
  );

  if (!open) return null;

  const toggleCamera = (id: string) => {
    const next = new Set(selectedCameras);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setSelectedCameras(next);
  };

  const togglePreset = (id: string) => {
    const next = new Set(selectedPresets);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setSelectedPresets(next);
  };

  const submit = async () => {
    if (jobCount === 0) return;
    setBusy(true);
    try {
      await onSubmit({
        cameraIds: [...selectedCameras],
        presetIds: [...selectedPresets],
      });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="batch-render"
      role="dialog"
      aria-modal="true"
      aria-label="Batch render"
      data-testid="batch-render-modal"
    >
      <header>
        <h2>Batch render</h2>
        <p>
          {jobCount} job{jobCount === 1 ? "" : "s"} will be queued (
          {selectedCameras.size} camera × {selectedPresets.size} preset).
        </p>
      </header>

      <section className="batch-render__group">
        <h3>Cameras</h3>
        {cameras.length === 0 ? (
          <p data-testid="batch-render-cameras-empty">
            No saved cameras. Save cameras from the viewport first.
          </p>
        ) : (
          <ul>
            {cameras.map((c) => (
              <li key={c.id}>
                <label>
                  <input
                    type="checkbox"
                    checked={selectedCameras.has(c.id)}
                    onChange={() => toggleCamera(c.id)}
                    data-testid={`batch-render-camera-${c.id}`}
                  />
                  {c.name}
                </label>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="batch-render__group">
        <h3>Presets</h3>
        <ul>
          {PRESET_IDS.map((id) => (
            <li key={id}>
              <label>
                <input
                  type="checkbox"
                  checked={selectedPresets.has(id)}
                  onChange={() => togglePreset(id)}
                  data-testid={`batch-render-preset-${id}`}
                />
                {id}
              </label>
            </li>
          ))}
        </ul>
      </section>

      <footer className="batch-render__actions">
        <button
          type="button"
          onClick={onCancel}
          data-testid="batch-render-cancel"
        >
          Cancel
        </button>
        <button
          type="button"
          disabled={busy || jobCount === 0}
          onClick={submit}
          data-testid="batch-render-submit"
        >
          {busy ? "Submitting…" : `Queue ${jobCount} render${jobCount === 1 ? "" : "s"}`}
        </button>
      </footer>
    </div>
  );
}
