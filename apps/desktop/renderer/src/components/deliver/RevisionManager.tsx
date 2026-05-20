/**
 * Revision manager sidebar — lists tagged revisions and lets the user
 * pick a base/head pair for the compare panel. The compare action
 * itself is wired up by the parent page; this component only owns
 * selection UI.
 */

import { useState } from "react";

import type { RevisionSummary, VersionDiffSummary } from "../../../../electron/bridge";

export interface RevisionManagerProps {
  revisions: RevisionSummary[];
  baseId: string | null;
  headId: string | null;
  onSelectBase: (id: string) => void;
  onSelectHead: (id: string) => void;
  onCreateRevision: (tag: string, description: string) => Promise<void>;
  onCompare: () => void;
  diff: VersionDiffSummary | null;
  comparing: boolean;
}

export function RevisionManager({
  revisions,
  baseId,
  headId,
  onSelectBase,
  onSelectHead,
  onCreateRevision,
  onCompare,
  diff,
  comparing,
}: RevisionManagerProps): JSX.Element {
  const [tag, setTag] = useState("");
  const [description, setDescription] = useState("");
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  const create = async () => {
    setCreating(true);
    setCreateError(null);
    try {
      await onCreateRevision(tag.trim(), description.trim());
      setTag("");
      setDescription("");
    } catch (err) {
      setCreateError(err instanceof Error ? err.message : String(err));
    } finally {
      setCreating(false);
    }
  };

  return (
    <aside data-testid="revision-manager">
      <header>
        <h2>Revisions</h2>
      </header>
      <form
        data-testid="revision-create-form"
        onSubmit={(e) => {
          e.preventDefault();
          void create();
        }}
      >
        <label>
          Tag
          <input
            type="text"
            value={tag}
            onChange={(e) => setTag(e.target.value)}
            placeholder="client-review-1"
            data-testid="revision-tag-input"
          />
        </label>
        <label>
          Description
          <input
            type="text"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            placeholder="Initial layout for client review"
            data-testid="revision-description-input"
          />
        </label>
        <button
          type="submit"
          disabled={creating || tag.trim().length === 0}
          data-testid="revision-create-button"
        >
          {creating ? "Tagging…" : "Tag revision"}
        </button>
        {createError ? (
          <p role="alert" data-testid="revision-create-error">
            {createError}
          </p>
        ) : null}
      </form>
      {revisions.length === 0 ? (
        <p data-testid="revision-empty">No revisions tagged yet.</p>
      ) : (
        <ul>
          {revisions.map((r) => {
            const isBase = r.revisionId === baseId;
            const isHead = r.revisionId === headId;
            return (
              <li
                key={r.revisionId}
                data-testid={`revision-entry-${r.revisionId}`}
              >
                <strong>{r.tag}</strong>
                <small>{new Date(r.createdAt).toLocaleString()}</small>
                {r.description ? <p>{r.description}</p> : null}
                <div>
                  <button
                    type="button"
                    aria-pressed={isBase}
                    onClick={() => onSelectBase(r.revisionId)}
                    data-testid={`revision-pick-base-${r.revisionId}`}
                  >
                    {isBase ? "Base ✓" : "Set as base"}
                  </button>
                  <button
                    type="button"
                    aria-pressed={isHead}
                    onClick={() => onSelectHead(r.revisionId)}
                    data-testid={`revision-pick-head-${r.revisionId}`}
                  >
                    {isHead ? "Head ✓" : "Set as head"}
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      )}
      <button
        type="button"
        disabled={
          comparing || baseId === null || headId === null || baseId === headId
        }
        onClick={onCompare}
        data-testid="revision-compare-button"
      >
        {comparing ? "Comparing…" : "Compare"}
      </button>
      {diff ? <DiffSummary diff={diff} /> : null}
    </aside>
  );
}

function DiffSummary({ diff }: { diff: VersionDiffSummary }): JSX.Element {
  const entries = Object.entries(diff.byCategory).sort(([a], [b]) =>
    a.localeCompare(b),
  );
  return (
    <section data-testid="revision-diff-summary">
      <h3>Diff</h3>
      <ul>
        {entries.map(([category, counts]) => (
          <li key={category} data-testid={`diff-category-${category}`}>
            <strong>{category}</strong>
            <span>
              {counts.added} added · {counts.removed} removed ·{" "}
              {counts.modified} modified · {counts.unchanged} unchanged
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}
