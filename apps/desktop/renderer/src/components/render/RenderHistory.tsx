import { useMemo, useState } from "react";
import {
  PublishCardModal,
  type PublishOutcome,
} from "../kchat/PublishCardModal";

export interface RenderHistoryEntry {
  jobId: string;
  presetId: string;
  cameraName?: string | null;
  completedAt: string; // ISO 8601
  outputPath: string;
  imageHash?: string | null;
  thumbnailHash?: string | null;
  durationMs: number;
}

interface Props {
  entries: RenderHistoryEntry[];
  selectedA: string | null;
  selectedB: string | null;
  onSelectA: (jobId: string | null) => void;
  onSelectB: (jobId: string | null) => void;
  /**
   * Optional project deep link prefix used when publishing renders
   * to KChat. Defaults to a synthetic `aecstudio://project/local/...`
   * scheme so the modal can be exercised in dev/test mode without
   * an open project. Real callers should pass
   * `aecstudio://project/<projectId>` so the artifact card's
   * `project_link` resolves to a real artifact.
   */
  projectLinkPrefix?: string;
}

/**
 * Timeline of completed renders with thumbnails. The user picks two
 * entries (A and B) which feed into the BeforeAfterCompare component.
 */
export function RenderHistory({
  entries,
  selectedA,
  selectedB,
  onSelectA,
  onSelectB,
  projectLinkPrefix = "aecstudio://project/local",
}: Props) {
  const sorted = useMemo(
    () =>
      [...entries].sort((a, b) =>
        b.completedAt.localeCompare(a.completedAt),
      ),
    [entries],
  );

  const [publishTarget, setPublishTarget] =
    useState<RenderHistoryEntry | null>(null);
  const [publishOutcome, setPublishOutcome] =
    useState<PublishOutcome | null>(null);

  if (sorted.length === 0) {
    return (
      <section
        className="render-history render-history--empty"
        data-testid="render-history"
      >
        <p>No completed renders yet.</p>
      </section>
    );
  }

  return (
    <section
      className="render-history"
      aria-label="Render history"
      data-testid="render-history"
    >
      <ol>
        {sorted.map((e) => {
          const isA = e.jobId === selectedA;
          const isB = e.jobId === selectedB;
          return (
            <li
              key={e.jobId}
              className={`render-history__row ${
                isA ? "render-history__row--a" : ""
              } ${isB ? "render-history__row--b" : ""}`}
              data-testid={`render-history-row-${e.jobId}`}
            >
              <div className="render-history__meta">
                <span className="render-history__job">{e.jobId}</span>
                <span className="render-history__preset">{e.presetId}</span>
                {e.cameraName && (
                  <span className="render-history__camera">
                    {e.cameraName}
                  </span>
                )}
                <time
                  className="render-history__time"
                  dateTime={e.completedAt}
                >
                  {formatTime(e.completedAt)}
                </time>
                <span className="render-history__duration">
                  {formatDuration(e.durationMs)}
                </span>
              </div>
              <div className="render-history__actions">
                <button
                  type="button"
                  data-testid={`render-history-pick-a-${e.jobId}`}
                  onClick={() => onSelectA(isA ? null : e.jobId)}
                  aria-pressed={isA}
                >
                  {isA ? "A ✓" : "A"}
                </button>
                <button
                  type="button"
                  data-testid={`render-history-pick-b-${e.jobId}`}
                  onClick={() => onSelectB(isB ? null : e.jobId)}
                  aria-pressed={isB}
                >
                  {isB ? "B ✓" : "B"}
                </button>
                <button
                  type="button"
                  data-testid={`render-history-publish-${e.jobId}`}
                  onClick={() => setPublishTarget(e)}
                  aria-label={`Publish render ${e.jobId} to KChat`}
                >
                  Publish
                </button>
              </div>
            </li>
          );
        })}
      </ol>
      {publishTarget && (
        <PublishCardModal
          artifactKind="concept_render"
          projectLink={`${projectLinkPrefix}/render/${publishTarget.jobId}`}
          defaultCaption={`${publishTarget.presetId} render of ${
            publishTarget.cameraName ?? "scene"
          }`}
          thumbnailBlake3={publishTarget.thumbnailHash ?? null}
          onClose={(outcome) => {
            setPublishTarget(null);
            setPublishOutcome(outcome);
          }}
        />
      )}
      {publishOutcome && publishOutcome.kind === "published" && (
        <p
          data-testid="render-history-publish-ack"
          className="render-history__publish-ack"
          role="status"
        >
          Published to thread <code>{publishOutcome.threadId}</code>
        </p>
      )}
      {publishOutcome && publishOutcome.kind === "failed" && (
        <p
          data-testid="render-history-publish-err"
          className="render-history__publish-err"
          role="alert"
        >
          Publish failed: {publishOutcome.message}
        </p>
      )}
    </section>
  );
}

function formatTime(iso: string): string {
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return iso;
  }
}

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const m = Math.floor(seconds / 60);
  const s = seconds - m * 60;
  return `${m}m${s.toString().padStart(2, "0")}s`;
}
