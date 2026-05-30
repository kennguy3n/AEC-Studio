import { useEffect, useRef, useState } from "react";
import type { ProjectSummary } from "../api/aec";
import { aec } from "../api/aec";

interface Props {
  project: ProjectSummary;
  onOpen?: (project: ProjectSummary) => void;
}

/**
 * Recent-project tile shown on the Home page. Phase 17 Group B
 * Task 12 added the real thumbnail fetch: on mount we ask the
 * bridge for the saved PNG blob via `aec.project.getThumbnail`
 * and, on hit, render it as an `<img>` in the `.project-card__thumb`
 * slot. On miss the gradient placeholder shows through.
 *
 * The PNG bytes come back as a `Uint8Array` (Node `Buffer` from the
 * native bridge, plain typed array from the in-process fallback —
 * both are `Uint8Array`-compatible). We wrap them in a `Blob` and
 * call `URL.createObjectURL`, which gives us a `blob:` URL the
 * `<img>` element can decode directly without a base64 hop. The
 * URL is revoked on unmount / when a new thumbnail arrives so we
 * don't leak per-card.
 */
export function ProjectCard({ project, onOpen }: Props) {
  const [thumbUri, setThumbUri] = useState<string | null>(null);
  // `[width, height]` of the saved thumbnail. Pinning these as
  // `<img>` attributes prevents the gradient slot from layout-
  // shifting when the image decodes after first paint.
  const [thumbDims, setThumbDims] = useState<[number, number] | null>(null);
  // Stable handle to the active `blob:` URL so the cleanup effect
  // can revoke it without depending on `thumbUri` (which would
  // re-run on every render).
  const lastObjectUrl = useRef<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const t = await aec.project.getThumbnail(project.path);
        if (cancelled) return;
        if (t === null) {
          // No saved thumbnail yet → fall back to the gradient
          // placeholder. Explicitly clear any stale URL from a
          // previous render of this card (e.g. a project that lost
          // its thumbnail row between mounts).
          if (lastObjectUrl.current) {
            URL.revokeObjectURL(lastObjectUrl.current);
            lastObjectUrl.current = null;
          }
          setThumbUri(null);
          setThumbDims(null);
          return;
        }
        // The bridge surfaces PNG bytes as `Uint8Array<ArrayBufferLike>`
        // (the cross-platform default — NAPI may hand back a Node
        // `Buffer` whose backing buffer type is widened beyond a
        // pure `ArrayBuffer`). The `Blob` constructor's `BlobPart`
        // type only accepts `ArrayBufferView<ArrayBuffer>` view, so
        // we copy the bytes into a fresh `Uint8Array` backed by a
        // dedicated `ArrayBuffer` before handing them off. This is
        // a one-time, per-thumbnail copy (~50 KB) and avoids a
        // `SharedArrayBuffer`-flavoured leak into the Blob API.
        const png = new Uint8Array(t.png.byteLength);
        png.set(t.png);
        const url = URL.createObjectURL(
          new Blob([png], { type: "image/png" }),
        );
        if (lastObjectUrl.current) {
          URL.revokeObjectURL(lastObjectUrl.current);
        }
        lastObjectUrl.current = url;
        setThumbUri(url);
        setThumbDims([t.width, t.height]);
      } catch {
        // The bridge surfaces validation / IO failures as rejected
        // promises. The card falls back to the gradient — a missing
        // thumbnail is a soft failure, not a reason to break the
        // Home grid.
        if (!cancelled) {
          setThumbUri(null);
          setThumbDims(null);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
    // Re-fetch when the project identity changes (Home grid is
    // reused across `closeProject` → `openProject` cycles) or when
    // the project's `modifiedAt` advances (a `projectSave` ran and
    // may have written a fresh thumbnail).
  }, [project.path, project.modifiedAt]);

  // Final unmount: revoke any URL we still own. Splitting this from
  // the fetch effect keeps the revoke off the dependency-driven
  // re-run path (a `modifiedAt` bump should NOT revoke the URL
  // before the new fetch resolves — the second `URL.revokeObjectURL`
  // inside the fetch handles the swap).
  useEffect(() => {
    return () => {
      if (lastObjectUrl.current) {
        URL.revokeObjectURL(lastObjectUrl.current);
        lastObjectUrl.current = null;
      }
    };
  }, []);

  return (
    <article className="card project-card" data-testid="project-card">
      <div className="project-card__thumb" aria-hidden>
        {thumbUri && (
          <img
            src={thumbUri}
            alt=""
            className="project-card__thumb-img"
            width={thumbDims?.[0]}
            height={thumbDims?.[1]}
            // Decoding asynchronously means the browser will not
            // block the main thread for a card that ends up
            // off-screen; combined with `loading="lazy"` this keeps
            // the recent-grid scroll smooth even when 50+ projects
            // are listed.
            decoding="async"
            loading="lazy"
            data-testid="project-card-thumb-img"
          />
        )}
      </div>
      <div>
        <div className="project-card__name">{project.name}</div>
        <div className="project-card__meta">
          {project.templateKey ? `${project.templateKey} · ` : ""}
          {formatRelative(project.modifiedAt)}
        </div>
      </div>
      <button
        type="button"
        className="button button--secondary"
        onClick={() => onOpen?.(project)}
      >
        Open
      </button>
    </article>
  );
}

function formatRelative(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  const delta = Date.now() - d.getTime();
  const minutes = Math.round(delta / 60_000);
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days} d ago`;
  return d.toLocaleDateString();
}
