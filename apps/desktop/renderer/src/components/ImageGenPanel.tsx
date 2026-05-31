/**
 * Phase 18 Group C Task 16 — Settings page subsection that surfaces
 * the single-model image-gen sidecar.
 *
 * Parallels {@link AiModelsSection} but image-gen is single-model
 * (no tier concept) — there is one configured GGUF descriptor at a
 * time. The first-run wizard pins it via `imageGen.setDescriptor`
 * (URL + BLAKE3 + size + filename). Subsequent downloads use that
 * descriptor; subsequent `generate` calls cold-spawn the sidecar on
 * first use, reuse it for ~120 s of idle, then unload.
 *
 * The Rust side (`crates/aec_bridge/src/service.rs`) is wired as a
 * peer to the text-side `AiState`: own `ImageGenRuntime`, own
 * `ImageGenModelManager`, own `Mutex<Option<ImageGenDownloadProgress>>`
 * progress slot. This component:
 *
 *   1. Fetches `imageGen:modelAvailability` on mount + after every
 *      action to render the "Downloaded" / "Not downloaded" badge.
 *   2. Polls `imageGen:downloadProgress` every 500 ms while a
 *      download is in-flight.
 *   3. Polls `imageGen:runtimeStatus` every 500 ms while a generate
 *      is in-flight so the user sees the loading → ready transition.
 *   4. Calls `imageGen:downloadModel` (long-running),
 *      `imageGen:generate` (cold-spawn + multi-second sampling).
 *
 * No telemetry, no analytics — every IPC stays on loopback. See
 * `docs/AI_RUNTIME.md` (Group F) for the full image-gen data-flow
 * diagram.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { aec } from "../api/aec";
import type {
  ImageGenDownloadProgress,
  ImageGenGenerateResult,
  ImageGenModelAvailability,
  ImageGenPolicy,
  ImageGenPresetEntry,
  ImageGenRuntimeStatus,
} from "../../../electron/bridge";

function formatBytes(n: number): string {
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(2)} GB`;
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${n} B`;
}

function pct(p: ImageGenDownloadProgress): number {
  if (p.total <= 0) return 0;
  return Math.min(100, Math.max(0, (p.downloaded / p.total) * 100));
}

/**
 * Reasonable defaults for the first `generate` request. The renderer
 * picks 512×512 / 20 steps / cfg=7 because that's the
 * stable-diffusion.cpp out-of-the-box sweet spot for an SD 1.5 GGUF
 * (~5 s on a recent Apple Silicon / NVIDIA GPU, ~30 s on CPU). The
 * user can edit any of these in the form before pressing Generate.
 */
const DEFAULT_WIDTH = 512;
const DEFAULT_HEIGHT = 512;
const DEFAULT_STEPS = 20;
const DEFAULT_CFG_SCALE = 7;

/**
 * Server-side request validation lives in
 * `crates/aec_bridge/src/service.rs::image_gen_generate`. We mirror
 * the same constraints client-side so the user sees normalized
 * values in the form *before* round-tripping through the bridge —
 * the bridge still re-validates as the authoritative gate (any
 * future API change updates only the bridge), but pushing a 511×511
 * request that's going to fail with "must be multiples of 8" wastes
 * the user's time on a 30 s cold spawn.
 *
 * Keep these in lockstep with the Rust-side numeric constants.
 */
const IMAGE_GEN_DIM_MIN = 64;
const IMAGE_GEN_DIM_MAX = 2048;
const IMAGE_GEN_DIM_STEP = 8;
const IMAGE_GEN_STEPS_MIN = 1;
const IMAGE_GEN_STEPS_MAX = 150;
const IMAGE_GEN_CFG_MIN = 0;
const IMAGE_GEN_CFG_MAX = 30;

/**
 * Snap `value` to the nearest multiple of `IMAGE_GEN_DIM_STEP` and
 * clamp into `[IMAGE_GEN_DIM_MIN, IMAGE_GEN_DIM_MAX]`. Used on
 * width/height blur + on submit so an in-progress edit ("typing 1024
 * one digit at a time") isn't fought on every keystroke while a
 * committed value is still guaranteed valid.
 */
function normalizeImageGenDimension(value: number): number {
  // NaN → MIN (no signal which direction the user meant). ±Infinity
  // falls through to the clamp below, which routes them to the
  // appropriate bound.
  if (Number.isNaN(value)) return IMAGE_GEN_DIM_MIN;
  const snapped =
    Math.round(value / IMAGE_GEN_DIM_STEP) * IMAGE_GEN_DIM_STEP;
  return Math.min(IMAGE_GEN_DIM_MAX, Math.max(IMAGE_GEN_DIM_MIN, snapped));
}

function clampImageGenSteps(value: number): number {
  if (Number.isNaN(value)) return IMAGE_GEN_STEPS_MIN;
  // Math.round(±Infinity) is ±Infinity; the clamp below handles it.
  return Math.min(
    IMAGE_GEN_STEPS_MAX,
    Math.max(IMAGE_GEN_STEPS_MIN, Math.round(value)),
  );
}

function clampImageGenCfg(value: number): number {
  if (Number.isNaN(value)) return IMAGE_GEN_CFG_MIN;
  return Math.min(IMAGE_GEN_CFG_MAX, Math.max(IMAGE_GEN_CFG_MIN, value));
}

/**
 * Exposed for tests + as defense-in-depth at submit time.
 */
export const __imageGenPanelTestables = {
  normalizeImageGenDimension,
  clampImageGenSteps,
  clampImageGenCfg,
  IMAGE_GEN_DIM_MIN,
  IMAGE_GEN_DIM_MAX,
  IMAGE_GEN_DIM_STEP,
  IMAGE_GEN_STEPS_MIN,
  IMAGE_GEN_STEPS_MAX,
  IMAGE_GEN_CFG_MIN,
  IMAGE_GEN_CFG_MAX,
};

export function ImageGenPanel(): JSX.Element {
  const [availability, setAvailability] =
    useState<ImageGenModelAvailability | null>(null);
  const [runtime, setRuntime] = useState<ImageGenRuntimeStatus | null>(null);
  const [progress, setProgress] = useState<ImageGenDownloadProgress | null>(
    null,
  );
  const [error, setError] = useState<string | null>(null);
  // Truthy while a download is in flight — disables the Download
  // button and starts the 500 ms progress poll.
  const [downloading, setDownloading] = useState(false);
  // Truthy while a generate request is in flight — disables the
  // Generate button and starts the 500 ms runtime poll so the user
  // sees "Loading model…" during the cold spawn.
  const [generating, setGenerating] = useState(false);
  // Last successful txt2img result. We keep the result in state so the
  // user can switch away from the Settings page and back without
  // losing what they generated.
  const [result, setResult] = useState<ImageGenGenerateResult | null>(null);
  // Suppresses the polling effect from clobbering a fresh failure
  // banner after the user dismisses it — mirrors `AiModelsSection`.
  const dismissedFailedRef = useRef(false);
  // Phase 18 Group C Task 17 — active governor policy + whether a
  // path-traced render is currently running. The two together drive
  // the "paused while rendering" banner + Generate-button gate. The
  // policy is read once on mount + after every
  // `governor.applyHardwareTier` (today the tier is set elsewhere
  // in Settings — when the wiring lands the panel will refresh via
  // an effect on the tier value). `renderInProgress` polls every
  // ~1 s while the panel is open.
  const [policy, setPolicy] = useState<ImageGenPolicy | null>(null);
  const [renderInProgress, setRenderInProgress] = useState(false);

  // Phase 18 Group D Task 20 — first-run wizard state. We load the
  // curated preset list once on mount; the registry is baked into
  // the binary so this is cheap (no HTTP, no disk I/O — just a
  // serde round-trip across the napi boundary). `presets === null`
  // means "the wizard has not finished loading yet" and renders the
  // loading spinner; `presets === []` is the intentional empty
  // ship-state where the wizard shows the manual-entry CTA only.
  const [presets, setPresets] = useState<ImageGenPresetEntry[] | null>(null);
  // Truthy while a `setDescriptor` is in flight in response to a
  // preset-pin click. Disables every preset button while the bridge
  // round-trip is in flight so the user can't double-tap a slow
  // network or queue two competing descriptors.
  const [pinningPresetId, setPinningPresetId] = useState<string | null>(null);

  // ---- form state ----
  const [prompt, setPrompt] = useState("");
  const [negativePrompt, setNegativePrompt] = useState("");
  const [width, setWidth] = useState(DEFAULT_WIDTH);
  const [height, setHeight] = useState(DEFAULT_HEIGHT);
  const [steps, setSteps] = useState(DEFAULT_STEPS);
  const [cfgScale, setCfgScale] = useState(DEFAULT_CFG_SCALE);
  const [seedText, setSeedText] = useState("");
  const [sampler, setSampler] = useState("");

  const refreshAvailability = useCallback(async () => {
    try {
      const a = await aec.imageGen.modelAvailability();
      setAvailability(a as ImageGenModelAvailability);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const refreshRuntime = useCallback(async () => {
    try {
      const r = await aec.imageGen.runtimeStatus();
      setRuntime(r as ImageGenRuntimeStatus);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void refreshAvailability();
    void refreshRuntime();
  }, [refreshAvailability, refreshRuntime]);

  // Phase 18 Group D Task 20 — first-run wizard: load the curated
  // preset list once on mount. The registry is compile-time
  // embedded so this is cheap; we don't refresh it because
  // `ai_models.json` only ever changes between releases. On
  // failure we surface to the top-level error banner so the
  // wizard's "preset section" gracefully degrades to the manual-
  // entry form.
  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const r = (await aec.imageGen.listPresets()) as ImageGenPresetEntry[];
        if (!cancelled) setPresets(r);
      } catch (e) {
        if (!cancelled) {
          // Fall back to "no curated presets" — the manual-entry
          // form is still usable. Don't clobber the top-level
          // error since this is a soft failure.
          setPresets([]);
          // eslint-disable-next-line no-console
          console.warn(
            "image-gen list_presets failed; falling back to manual entry",
            e,
          );
        }
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, []);

  // Phase 18 Group C Task 17 — policy snapshot on mount + slow
  // refresh while mounted. The policy only changes on
  // `governor.applyHardwareTier`, which is a deliberate user
  // action elsewhere in Settings, so a 3 s cadence is more than
  // fast enough to pick up tier swaps without burning IPC. We
  // can't share the `renderInProgress` tick because the policy
  // determines *whether* we even need that tick — the gating
  // effect below depends on `policy.allowDuringPathtracedRender`.
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setInterval> | null = null;
    const tick = async () => {
      try {
        const p = await aec.imageGen.activePolicy();
        if (!cancelled) setPolicy(p as ImageGenPolicy);
      } catch {
        // Swallow — the panel still functions with `policy = null`
        // (which defaults to "no banner" / generate enabled). The
        // server-side gate is the source of truth.
      }
    };
    void tick();
    timer = setInterval(() => void tick(), 3000);
    return () => {
      cancelled = true;
      if (timer !== null) clearInterval(timer);
    };
  }, []);

  // Phase 18 Group C Task 17 — poll the render-queue inspector
  // every 1 s while the panel is mounted, *but only on tiers
  // whose policy actually pauses image-gen during a path-traced
  // render*. On High / Pro (`allowDuringPathtracedRender = true`)
  // the banner is never shown and the Generate button is never
  // gated by the render state, so polling there is pure waste —
  // both IPC and a wakeup every second on a likely-idle settings
  // tab. We resolve `shouldPoll` after `policy` is first
  // populated; until then we err on the side of polling so the
  // very first tick on Low / Medium still reflects an in-progress
  // render that started before the panel opened. The dependency
  // on `policy` re-runs this effect when the tier changes, so
  // moving Low → Pro stops the tick and Pro → Low restarts it.
  const shouldPollRenderQueue =
    policy === null || policy.allowDuringPathtracedRender !== true;
  useEffect(() => {
    if (!shouldPollRenderQueue) {
      // Tier doesn't gate generate on render — clear any stale
      // banner state and don't burn IPC on a poll we won't use.
      setRenderInProgress(false);
      return;
    }
    let cancelled = false;
    let timer: ReturnType<typeof setInterval> | null = null;
    const tick = async () => {
      try {
        const v = await aec.render.pathtracedInProgress();
        if (!cancelled) setRenderInProgress(Boolean(v));
      } catch {
        // Swallow — banner falls back to "no render".
      }
    };
    void tick();
    timer = setInterval(() => void tick(), 1000);
    return () => {
      cancelled = true;
      if (timer !== null) clearInterval(timer);
    };
  }, [shouldPollRenderQueue]);

  // Progress poll. Mirrors `AiModelsSection` — only runs while a
  // download is in flight to keep idle Settings sessions IPC-quiet.
  useEffect(() => {
    if (!downloading) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const p =
          (await aec.imageGen.downloadProgress()) as ImageGenDownloadProgress | null;
        if (cancelled) return;
        if (p) {
          if (p.state === "failed" && dismissedFailedRef.current) {
            return;
          }
          setProgress(p);
          if (p.state === "completed" || p.state === "failed") {
            setDownloading(false);
            void refreshAvailability();
          }
        }
      } catch (e) {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : String(e));
        }
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), 500);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [downloading, refreshAvailability]);

  // Runtime status poll. Drives the "Loading model…" indicator
  // during the cold spawn (~10–30 s). Active only while a generate
  // is in flight.
  useEffect(() => {
    if (!generating) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const r = await aec.imageGen.runtimeStatus();
        if (cancelled) return;
        setRuntime(r as ImageGenRuntimeStatus);
      } catch {
        // Status poll errors are non-fatal — the generate call
        // itself will surface real failures.
      }
    };
    void tick();
    const id = window.setInterval(() => void tick(), 500);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [generating]);

  // Phase 18 Group D Task 20 — pin a curated preset's descriptor.
  // The wizard's "one-click pin" UX: clicking a preset entry fires
  // `imageGen.setDescriptor` with the embedded
  // `ImageGenModelDescriptor`. After a successful pin we refresh
  // the availability snapshot so the rest of the panel transitions
  // out of the `noDescriptor` branch into the "ready to download"
  // branch. The pinned descriptor only sets the configured model
  // path; the user still needs to click Download next to fetch the
  // actual GGUF bytes.
  const onPinPreset = useCallback(
    async (p: ImageGenPresetEntry) => {
      setError(null);
      setPinningPresetId(p.id);
      try {
        await aec.imageGen.setDescriptor({
          filename: p.filename,
          sizeBytes: p.sizeBytes,
          blake3Hex: p.blake3Hex,
          downloadUrl: p.downloadUrl,
          vaeFilename: p.vaeFilename,
        });
        await refreshAvailability();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setPinningPresetId(null);
      }
    },
    [refreshAvailability],
  );

  const onDownload = useCallback(async () => {
    setError(null);
    dismissedFailedRef.current = false;
    setProgress(null);
    setDownloading(true);
    try {
      await aec.imageGen.downloadModel();
      // Bridge resolved → file is BLAKE3-verified.
      setDownloading(false);
    } catch (e) {
      // Same dual-banner avoidance pattern as `AiModelsSection`: the
      // Rust side publishes `DownloadState::Failed` before rejecting
      // the napi promise. Pull the progress slot eagerly; if it
      // shows Failed we route through the per-tier (per-model) banner
      // and skip the top-level error.
      const msg = e instanceof Error ? e.message : String(e);
      let routedToPanel = false;
      try {
        const p =
          (await aec.imageGen.downloadProgress()) as ImageGenDownloadProgress | null;
        if (p && p.state === "failed") {
          setProgress(p);
          routedToPanel = true;
        }
      } catch {
        // Ignore — fall through to the top-level banner.
      }
      if (!routedToPanel) setError(msg);
      setDownloading(false);
    } finally {
      await refreshAvailability();
    }
  }, [refreshAvailability]);

  const onGenerate = useCallback(async () => {
    setError(null);
    if (prompt.trim().length === 0) {
      setError("Prompt must not be empty.");
      return;
    }
    setGenerating(true);
    try {
      const seedValue =
        seedText.trim().length === 0 ? null : Number(seedText.trim());
      if (seedValue !== null && !Number.isFinite(seedValue)) {
        throw new Error("Seed must be an integer (or empty for random).");
      }
      if (seedValue !== null && !Number.isInteger(seedValue)) {
        throw new Error("Seed must be an integer (or empty for random).");
      }
      // Defense-in-depth: re-normalize numeric fields at submit
      // time. The onBlur handlers already do this for "user clicks
      // away from the field", but if the user types a value and
      // hits Enter without blurring, the in-state value is still
      // the raw onChange value (which only clamps to >= 1). Doing
      // the snap here too means the bridge always receives a
      // server-acceptable request — and the snapped values get
      // pushed back into form state below so the user sees what
      // was actually sent.
      const normalizedWidth = normalizeImageGenDimension(width);
      const normalizedHeight = normalizeImageGenDimension(height);
      const normalizedSteps = clampImageGenSteps(steps);
      const normalizedCfg = clampImageGenCfg(cfgScale);
      if (normalizedWidth !== width) setWidth(normalizedWidth);
      if (normalizedHeight !== height) setHeight(normalizedHeight);
      if (normalizedSteps !== steps) setSteps(normalizedSteps);
      if (normalizedCfg !== cfgScale) setCfgScale(normalizedCfg);

      const r = await aec.imageGen.generate({
        prompt,
        negativePrompt:
          negativePrompt.trim().length === 0 ? null : negativePrompt,
        width: normalizedWidth,
        height: normalizedHeight,
        steps: normalizedSteps,
        cfgScale: normalizedCfg,
        seed: seedValue,
        sampler: sampler.trim().length === 0 ? null : sampler,
      });
      setResult(r as ImageGenGenerateResult);
      // The sidecar reports the seed it actually used — pin it back
      // into the form so the user can re-roll the exact image.
      if (r.seed !== null && r.seed !== undefined) {
        setSeedText(String(r.seed));
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setGenerating(false);
      await refreshRuntime();
    }
  }, [
    prompt,
    negativePrompt,
    width,
    height,
    steps,
    cfgScale,
    seedText,
    sampler,
    refreshRuntime,
  ]);

  if (!availability) {
    return (
      <section
        className="settings-section"
        data-testid="settings-section-image-gen"
        aria-label="Image generation"
      >
        <h2>Image generation</h2>
        <p>Loading model availability…</p>
      </section>
    );
  }

  const noDescriptor =
    availability.filename === "" && availability.downloadUrl === null;
  const downloadFailed = progress?.state === "failed";
  // Phase 18 Group C Task 17 — render gate. Tracks the server-side
  // policy: on tiers with `allowDuringPathtracedRender = false`
  // (Low / Medium today) the panel disables the Generate button
  // and renders a banner explaining the gate. The Rust side
  // returns `BridgeServiceError::ImageGen` if the user races the
  // gate (banner not yet visible) so this is a UX hint, not a
  // security boundary.
  const gatedByRender =
    policy !== null &&
    !policy.allowDuringPathtracedRender &&
    renderInProgress;

  return (
    <section
      className="settings-section"
      data-testid="settings-section-image-gen"
      aria-label="Image generation"
    >
      <h2>Image generation</h2>
      <p>
        Local stable-diffusion.cpp sidecar. The GGUF model is downloaded
        from the URL pinned by the first-run wizard, BLAKE3-verified, and
        used to cold-spawn an sd-server child process on the first
        Generate. The sidecar idles out after 120 s.
      </p>
      <p
        className="settings-models-dir"
        data-testid="settings-image-gen-models-dir"
      >
        Models directory:{" "}
        <code>{availability.modelsDir || "(not set)"}</code>
      </p>
      {error && (
        <div
          role="alert"
          className="settings-error"
          data-testid="settings-image-gen-error"
        >
          {error}
        </div>
      )}
      {noDescriptor && (
        <div
          role="status"
          className="settings-status"
          data-testid="settings-image-gen-no-descriptor"
        >
          No image-gen model is configured. Complete the first-run wizard
          to pin a model.
        </div>
      )}
      {/* Phase 18 Group D Task 20 — first-run wizard. Renders only
          when no descriptor is pinned (the wizard is for "first
          run" only; once pinned, the user can swap via the
          Download row's per-model controls). The wizard always
          shows even when `presets === []` so the user has clear
          visual confirmation that no curated entries are offered
          this build — falling back to the manual-entry path. */}
      {noDescriptor && (
        <div
          className="settings-image-gen-wizard"
          data-testid="settings-image-gen-wizard"
        >
          <h3>Choose an image-gen model</h3>
          {presets === null && (
            <p data-testid="settings-image-gen-wizard-loading">
              Loading curated presets…
            </p>
          )}
          {presets !== null && presets.length === 0 && (
            <p
              className="settings-image-gen-wizard__empty"
              data-testid="settings-image-gen-wizard-empty"
            >
              No curated presets are shipped in this build. Use the
              manual descriptor entry below to pin a model URL +
              BLAKE3 hash by hand.
            </p>
          )}
          {presets !== null && presets.length > 0 && (
            <ul
              className="settings-image-gen-wizard__list"
              data-testid="settings-image-gen-wizard-list"
            >
              {presets.map((p) => (
                <li
                  key={p.id}
                  className="settings-image-gen-wizard__row"
                  data-testid={`settings-image-gen-wizard-row-${p.id}`}
                >
                  <div className="settings-image-gen-wizard__meta">
                    <strong>{p.displayName}</strong>
                    <span className="settings-image-gen-wizard__filename">
                      <code>{p.filename}</code> ·{" "}
                      {formatBytes(p.sizeBytes)}
                    </span>
                  </div>
                  <button
                    type="button"
                    onClick={() => void onPinPreset(p)}
                    disabled={pinningPresetId !== null}
                    data-testid={`settings-image-gen-wizard-pin-${p.id}`}
                  >
                    {pinningPresetId === p.id ? "Pinning…" : "Pin"}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {!noDescriptor && (
        <div
          className="settings-models-row"
          data-testid="settings-image-gen-model"
          data-available={availability.available ? "true" : "false"}
        >
          <div className="settings-models-row__meta">
            <strong>{availability.filename}</strong>
            <span className="settings-models-row__filename">
              <code>{availability.filename}</code> ·{" "}
              {formatBytes(availability.sizeBytes)}
            </span>
          </div>
          <div className="settings-models-row__status">
            {availability.available ? (
              <span data-testid="settings-image-gen-available">
                Downloaded ({formatBytes(availability.sizeOnDisk)})
              </span>
            ) : (
              <span data-testid="settings-image-gen-missing">
                Not downloaded
              </span>
            )}
          </div>
          <div className="settings-models-row__actions">
            {!availability.available && (
              <button
                type="button"
                data-testid="settings-image-gen-download"
                disabled={downloading}
                onClick={() => void onDownload()}
              >
                {progress?.state === "downloading"
                  ? "Downloading…"
                  : progress?.state === "verifying"
                    ? "Verifying…"
                    : "Download"}
              </button>
            )}
          </div>
        </div>
      )}
      {(progress?.state === "downloading" || progress?.state === "verifying") &&
        progress && (
          <progress
            data-testid="settings-image-gen-progress"
            value={pct(progress)}
            max={100}
            aria-label={`Downloading ${progress.filename}: ${pct(progress).toFixed(0)}%`}
          >
            {pct(progress).toFixed(0)}%
          </progress>
        )}
      {downloadFailed && progress && (
        <div
          role="alert"
          className="settings-error"
          data-testid="settings-image-gen-failed"
        >
          Download failed: {progress.message ?? "unknown error"}
          <button
            type="button"
            onClick={() => {
              dismissedFailedRef.current = true;
              setProgress(null);
            }}
          >
            Dismiss
          </button>
        </div>
      )}

      {gatedByRender && (
        <div
          role="status"
          className="settings-status"
          data-testid="settings-image-gen-gated"
        >
          Image generation is paused while a path-traced render is
          running. Cancel or wait for the render queue to finish, or
          switch to a higher hardware tier in Settings to allow
          concurrent execution.
        </div>
      )}
      {availability.available && (
        <div
          className="settings-image-gen-form"
          data-testid="settings-image-gen-form"
        >
          <h3>Generate</h3>
          <label>
            Prompt
            <textarea
              data-testid="settings-image-gen-prompt"
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              rows={3}
              disabled={generating}
            />
          </label>
          <label>
            Negative prompt (optional)
            <textarea
              data-testid="settings-image-gen-negative-prompt"
              value={negativePrompt}
              onChange={(e) => setNegativePrompt(e.target.value)}
              rows={2}
              disabled={generating}
            />
          </label>
          <div className="settings-image-gen-form__row">
            <label>
              Width
              <input
                type="number"
                data-testid="settings-image-gen-width"
                value={width}
                min={IMAGE_GEN_DIM_MIN}
                max={IMAGE_GEN_DIM_MAX}
                step={IMAGE_GEN_DIM_STEP}
                disabled={generating}
                onChange={(e) =>
                  setWidth(Math.max(1, Number(e.target.value) || 0))
                }
                // Snap-to-multiple-of-8 + range clamp happens on
                // blur (not on every keystroke), so the user can
                // type "1024" one digit at a time without the field
                // jumping. Defense-in-depth normalization also runs
                // at submit time inside `onGenerate`.
                onBlur={(e) =>
                  setWidth(
                    normalizeImageGenDimension(Number(e.target.value) || 0),
                  )
                }
              />
            </label>
            <label>
              Height
              <input
                type="number"
                data-testid="settings-image-gen-height"
                value={height}
                min={IMAGE_GEN_DIM_MIN}
                max={IMAGE_GEN_DIM_MAX}
                step={IMAGE_GEN_DIM_STEP}
                disabled={generating}
                onChange={(e) =>
                  setHeight(Math.max(1, Number(e.target.value) || 0))
                }
                onBlur={(e) =>
                  setHeight(
                    normalizeImageGenDimension(Number(e.target.value) || 0),
                  )
                }
              />
            </label>
            <label>
              Steps
              <input
                type="number"
                data-testid="settings-image-gen-steps"
                value={steps}
                min={IMAGE_GEN_STEPS_MIN}
                max={IMAGE_GEN_STEPS_MAX}
                step={1}
                disabled={generating}
                onChange={(e) =>
                  setSteps(Math.max(1, Number(e.target.value) || 0))
                }
                onBlur={(e) =>
                  setSteps(clampImageGenSteps(Number(e.target.value) || 0))
                }
              />
            </label>
            <label>
              CFG
              <input
                type="number"
                data-testid="settings-image-gen-cfg"
                value={cfgScale}
                min={IMAGE_GEN_CFG_MIN}
                max={IMAGE_GEN_CFG_MAX}
                step={0.5}
                disabled={generating}
                onChange={(e) => setCfgScale(Number(e.target.value) || 0)}
                onBlur={(e) =>
                  setCfgScale(clampImageGenCfg(Number(e.target.value) || 0))
                }
              />
            </label>
          </div>
          <div className="settings-image-gen-form__row">
            <label>
              Seed (blank = random)
              <input
                type="text"
                inputMode="numeric"
                pattern="-?[0-9]*"
                data-testid="settings-image-gen-seed"
                value={seedText}
                disabled={generating}
                onChange={(e) => setSeedText(e.target.value)}
              />
            </label>
            <label>
              Sampler (blank = default)
              <input
                type="text"
                data-testid="settings-image-gen-sampler"
                value={sampler}
                disabled={generating}
                onChange={(e) => setSampler(e.target.value)}
              />
            </label>
          </div>
          <button
            type="button"
            data-testid="settings-image-gen-generate"
            disabled={
              generating || prompt.trim().length === 0 || gatedByRender
            }
            onClick={() => void onGenerate()}
          >
            {gatedByRender
              ? "Paused — render in progress"
              : generating
                ? runtime?.state === "loading"
                  ? "Loading model…"
                  : "Generating…"
                : "Generate"}
          </button>
          {result && (
            <div
              className="settings-image-gen-result"
              data-testid="settings-image-gen-result"
            >
              <img
                alt={`Generated: ${prompt.slice(0, 80)}`}
                src={`data:image/png;base64,${result.pngBase64}`}
                width={result.width}
                height={result.height}
              />
              <p
                className="settings-image-gen-result__meta"
                data-testid="settings-image-gen-result-meta"
              >
                {result.width}×{result.height} · {result.steps} steps · seed{" "}
                {result.seed ?? "(unknown)"}
                {result.info && (
                  <>
                    {" · "}
                    <code>{result.info}</code>
                  </>
                )}
              </p>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
