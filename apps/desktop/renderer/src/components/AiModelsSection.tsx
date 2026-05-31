/**
 * Phase 18 Group A Task 3 — Settings page subsection that surfaces
 * the three Ternary-Bonsai 1.58-bit text-model tiers.
 *
 * The Rust side (`crates/aec_bridge/src/service.rs::ai_download_model`)
 * runs each download on the tokio blocking thread pool and publishes
 * progress into a `Mutex<Option<AiDownloadProgress>>` slot. This
 * component:
 *
 *   1. Fetches `ai:modelAvailability` on mount + after every action
 *      to render the per-tier "Downloaded" / "Not downloaded" badge.
 *   2. Polls `ai:downloadProgress` every 500 ms while a download is
 *      in-flight so the progress bar updates without the renderer
 *      holding any long-lived connection.
 *   3. Calls `ai:downloadModel` (async, resolves when the file is
 *      BLAKE3-verified) and `ai:setActiveTier` (sync write).
 *
 * No telemetry, no analytics — every IPC stays on loopback. See
 * `docs/AI_RUNTIME.md` (Group F) for the full data-flow diagram.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { aec } from "../api/aec";
import type {
  AiModelAvailability,
  AiModelTierSlug,
  AiDownloadProgress,
} from "../../../electron/bridge";

function formatBytes(n: number): string {
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(2)} GB`;
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${n} B`;
}

function pct(p: AiDownloadProgress): number {
  if (p.total <= 0) return 0;
  return Math.min(100, Math.max(0, (p.downloaded / p.total) * 100));
}

export function AiModelsSection(): JSX.Element {
  const [availability, setAvailability] = useState<AiModelAvailability | null>(
    null,
  );
  const [progress, setProgress] = useState<AiDownloadProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Tier whose download button was last clicked — used to disable the
  // other rows' buttons while one is in flight (the Rust side
  // serialises downloads via the `ModelManager` mutex anyway, but a
  // grey button is a clearer signal than a "download already in
  // progress" error toast).
  const [pendingTier, setPendingTier] = useState<AiModelTierSlug | null>(null);
  // Suppresses the polling effect from clobbering a fresh failure
  // banner after the user dismisses it.
  const dismissedFailedRef = useRef(false);

  const refreshAvailability = useCallback(async () => {
    try {
      const a = await aec.ai.modelAvailability();
      setAvailability(a as AiModelAvailability);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void refreshAvailability();
  }, [refreshAvailability]);

  useEffect(() => {
    // Only poll while a download is in flight. The Rust slot keeps
    // the last terminal state ("completed" / "failed") between
    // sessions of this component too, so an interval that runs only
    // when `pendingTier` is set keeps idle Settings sessions free of
    // IPC noise.
    if (!pendingTier) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const p = (await aec.ai.downloadProgress()) as
          | AiDownloadProgress
          | null;
        if (cancelled) return;
        if (p) {
          if (p.state === "failed" && dismissedFailedRef.current) {
            return;
          }
          setProgress(p);
          if (p.state === "completed" || p.state === "failed") {
            setPendingTier(null);
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
  }, [pendingTier, refreshAvailability]);

  const onDownload = useCallback(
    async (tier: AiModelTierSlug) => {
      setError(null);
      dismissedFailedRef.current = false;
      setProgress(null);
      setPendingTier(tier);
      try {
        await aec.ai.downloadModel(tier);
        // Bridge resolved → file is BLAKE3-verified and on disk.
        // Clear `pendingTier` immediately so the Download buttons
        // re-enable on the next render rather than waiting up to
        // 500 ms for the polling effect's next tick to observe the
        // "completed" snapshot.
        setPendingTier(null);
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
        setPendingTier(null);
      } finally {
        await refreshAvailability();
      }
    },
    [refreshAvailability],
  );

  const onSetActive = useCallback(
    async (tier: AiModelTierSlug) => {
      setError(null);
      try {
        await aec.ai.setActiveTier(tier);
        await refreshAvailability();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [refreshAvailability],
  );

  if (!availability) {
    return (
      <section
        className="settings-section"
        data-testid="settings-section-ai-models"
        aria-label="AI text models"
      >
        <h2>AI text models</h2>
        <p>Loading model availability…</p>
      </section>
    );
  }

  return (
    <section
      className="settings-section"
      data-testid="settings-section-ai-models"
      aria-label="AI text models"
    >
      <h2>AI text models</h2>
      <p>
        Ternary-Bonsai 1.58-bit GGUF models. Downloads come from
        HuggingFace over HTTPS and are BLAKE3-verified.
      </p>
      <p
        className="settings-models-dir"
        data-testid="settings-ai-models-dir"
      >
        Models directory: <code>{availability.modelsDir || "(not set)"}</code>
      </p>
      {error && (
        <div
          role="alert"
          className="settings-error"
          data-testid="settings-ai-models-error"
        >
          {error}
        </div>
      )}
      <ul className="settings-models-list" data-testid="settings-ai-models-list">
        {availability.tiers.map((t) => {
          const isActive = availability.activeTier === t.tier;
          const isDownloading =
            pendingTier === t.tier &&
            progress?.state === "downloading" &&
            progress.tier === t.tier;
          const isVerifying =
            pendingTier === t.tier &&
            progress?.state === "verifying" &&
            progress.tier === t.tier;
          const failed =
            progress?.state === "failed" && progress.tier === t.tier;
          const buttonDisabled =
            t.available || pendingTier !== null;
          return (
            <li
              key={t.tier}
              data-testid={`settings-ai-model-${t.tier}`}
              data-active={isActive ? "true" : "false"}
              data-available={t.available ? "true" : "false"}
            >
              <div className="settings-models-row">
                <div className="settings-models-row__meta">
                  <strong>{t.name}</strong>
                  <span className="settings-models-row__filename">
                    <code>{t.filename}</code> · {formatBytes(t.sizeBytes)}
                  </span>
                </div>
                <div className="settings-models-row__status">
                  {t.available ? (
                    <span data-testid={`settings-ai-model-${t.tier}-available`}>
                      Downloaded ({formatBytes(t.sizeOnDisk)})
                    </span>
                  ) : (
                    <span data-testid={`settings-ai-model-${t.tier}-missing`}>
                      Not downloaded
                    </span>
                  )}
                </div>
                <div className="settings-models-row__actions">
                  {!t.available && (
                    <button
                      type="button"
                      data-testid={`settings-ai-model-${t.tier}-download`}
                      disabled={buttonDisabled}
                      onClick={() => void onDownload(t.tier)}
                    >
                      {isDownloading
                        ? "Downloading…"
                        : isVerifying
                          ? "Verifying…"
                          : "Download"}
                    </button>
                  )}
                  {t.available && (
                    <button
                      type="button"
                      data-testid={`settings-ai-model-${t.tier}-activate`}
                      disabled={isActive || pendingTier !== null}
                      onClick={() => void onSetActive(t.tier)}
                    >
                      {isActive ? "Active" : "Set active"}
                    </button>
                  )}
                </div>
              </div>
              {(isDownloading || isVerifying) && progress && (
                <progress
                  data-testid={`settings-ai-model-${t.tier}-progress`}
                  value={pct(progress)}
                  max={100}
                  aria-label={`Downloading ${t.name}: ${pct(progress).toFixed(0)}%`}
                >
                  {pct(progress).toFixed(0)}%
                </progress>
              )}
              {failed && progress && (
                <div
                  role="alert"
                  className="settings-error"
                  data-testid={`settings-ai-model-${t.tier}-failed`}
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
            </li>
          );
        })}
      </ul>
    </section>
  );
}
