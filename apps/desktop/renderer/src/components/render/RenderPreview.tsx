/**
 * Phase 17 Group C Task 17 — progressive in-flight render preview.
 *
 * Renders a single hero image (the most recently completed render's
 * PNG, base64-encoded by the page-level effect). When a render is
 * in flight, the page passes an `inFlight` descriptor and we overlay
 * a "Rendering · 42%" badge + progress bar on top of the previous
 * frame. The previous frame stays visible so the user has spatial
 * continuity between successive renders (artist workflow: tweak a
 * material → rerender → compare to the previous frame). When no
 * previous frame exists yet, the overlay sits on the empty-state
 * panel.
 *
 * The path tracer's writer only persists `outputPath` on completion
 * (Phase 17 Group A's behaviour, unchanged here), so a true
 * tile-streamed live preview would require a new bridge IPC and
 * shared-memory frame surface — that is the explicit scope of
 * Phase 17 Group D Task 21 (shared-memory viewport frames). For
 * Group C we surface the in-flight status visually without
 * inventing a tile-streamed surface.
 */

interface InFlight {
  jobId: string;
  /** Percent in `[0, 100]`. */
  progress: number;
  /** Preset id (e.g. `aec.preset.standard`) — surfaced in the overlay. */
  preset?: string;
}

interface Props {
  imageDataUri: string | null;
  caption?: string;
  inFlight?: InFlight | null;
}

export function RenderPreview({ imageDataUri, caption, inFlight }: Props) {
  const showOverlay = inFlight !== null && inFlight !== undefined;
  return (
    <figure
      className="render-preview"
      aria-label="Render preview"
      data-testid="render-preview"
      style={{ position: "relative" }}
    >
      {imageDataUri ? (
        <img
          src={imageDataUri}
          alt={caption ?? "Latest render"}
          data-testid="render-preview-image"
        />
      ) : (
        <div className="render-preview__empty" data-testid="render-preview-empty">
          <p>No preview yet. Queue a render or load a realtime preview.</p>
        </div>
      )}
      {showOverlay && (
        <div
          className="render-preview__inflight"
          data-testid="render-preview-inflight"
          style={overlayStyle}
        >
          <div style={{ fontSize: 12, fontWeight: 600 }}>
            Rendering · {Math.round(inFlight!.progress)}%
          </div>
          <div style={{ fontSize: 11, marginTop: 2, opacity: 0.85 }}>
            Job {inFlight!.jobId}
            {inFlight!.preset ? ` · ${inFlight!.preset}` : ""}
          </div>
          <progress
            value={Math.max(0, Math.min(100, inFlight!.progress))}
            max={100}
            data-testid="render-preview-inflight-progress"
            style={{ width: "100%", marginTop: 4, height: 4 }}
          />
        </div>
      )}
      {caption && <figcaption>{caption}</figcaption>}
    </figure>
  );
}

const overlayStyle: React.CSSProperties = {
  position: "absolute",
  top: 8,
  left: 8,
  right: 8,
  padding: "6px 10px",
  background: "rgba(0, 0, 0, 0.55)",
  color: "white",
  borderRadius: 6,
  pointerEvents: "none",
};
