import { useState } from "react";
import { aec } from "../../api/aec";

/**
 * Modal dialog that publishes a single AEC Studio artifact card to
 * KChat. Real props (not a placeholder shell):
 *
 * - `artifactKind` — concept_render / sheet_set / asset_pack /
 *   revision_pack. Renders the kind chip in the form header so the
 *   user is never confused about which artifact will be published.
 * - `projectLink` — the `aecstudio://project/<id>/<resource>/<id>`
 *   deep link that points back at the original artifact. The
 *   bridge enforces the scheme; this modal trusts the caller.
 * - `defaultCaption` / `thumbnailBlake3` — pre-populated by the
 *   caller (e.g. RenderHistory's "Publish" button passes the
 *   render's thumbnail hash and an auto-generated caption).
 * - `onClose` — called with the publish result when the user
 *   confirms, or `null` if they cancel.
 *
 * The form posts to `kchat:publish` via the bridge. Publishing is
 * disabled when the status chip reports `disconnected` AND the
 * publisher kind is `in_memory` — the in-memory publisher accepts
 * cards but they never leave the local process, which would be
 * surprising. The caller can override with `forcePublish={true}`
 * for tests / dev mode that explicitly want the in-memory round
 * trip.
 */
export type PublishCardModalProps = {
  artifactKind:
    | "concept_render"
    | "sheet_set"
    | "asset_pack"
    | "revision_pack";
  projectLink: string;
  defaultCaption: string;
  thumbnailBlake3: string | null;
  forcePublish?: boolean;
  onClose: (result: PublishOutcome) => void;
};

export type PublishOutcome =
  | { kind: "cancelled" }
  | {
      kind: "published";
      messageId: string;
      threadId: string;
      publishedAt: string;
    }
  | { kind: "failed"; message: string };

export function PublishCardModal(props: PublishCardModalProps) {
  const [caption, setCaption] = useState(props.defaultCaption);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const onSubmit = async () => {
    setError(null);
    setPending(true);
    try {
      const card = {
        artifact: props.artifactKind,
        caption,
        project_link: props.projectLink,
        thumbnail_blake3: props.thumbnailBlake3,
        metadata: {},
      };
      const result = await aec.kchat.publish({
        cardJson: JSON.stringify(card),
      });
      props.onClose({
        kind: "published",
        messageId: result.messageId,
        threadId: result.threadId,
        publishedAt: result.publishedAt,
      });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      props.onClose({ kind: "failed", message: msg });
    } finally {
      setPending(false);
    }
  };

  return (
    <div
      role="dialog"
      aria-modal="true"
      data-testid="kchat-publish-modal"
      aria-labelledby="kchat-publish-title"
      className="kchat-publish-modal"
    >
      <h2 id="kchat-publish-title">Publish to KChat</h2>
      <p
        className="kchat-publish-modal__kind"
        data-testid="kchat-publish-kind"
      >
        Artifact: <strong>{prettyKind(props.artifactKind)}</strong>
      </p>
      <label className="kchat-publish-modal__field">
        <span>Caption</span>
        <textarea
          data-testid="kchat-publish-caption"
          value={caption}
          maxLength={2_048}
          rows={3}
          onChange={(e) => setCaption(e.target.value)}
        />
      </label>
      {props.thumbnailBlake3 && (
        <p
          className="kchat-publish-modal__hash"
          data-testid="kchat-publish-thumbnail"
        >
          Thumbnail: <code>{props.thumbnailBlake3}</code>
        </p>
      )}
      {error && (
        <p role="alert" data-testid="kchat-publish-error">
          {error}
        </p>
      )}
      <div className="kchat-publish-modal__actions">
        <button
          type="button"
          data-testid="kchat-publish-cancel"
          disabled={pending}
          onClick={() => props.onClose({ kind: "cancelled" })}
        >
          Cancel
        </button>
        <button
          type="button"
          data-testid="kchat-publish-submit"
          disabled={pending || caption.trim().length === 0}
          onClick={() => void onSubmit()}
        >
          {pending ? "Publishing…" : "Publish"}
        </button>
      </div>
    </div>
  );
}

function prettyKind(k: PublishCardModalProps["artifactKind"]): string {
  switch (k) {
    case "concept_render":
      return "Concept render";
    case "sheet_set":
      return "Sheet set";
    case "asset_pack":
      return "Asset pack";
    case "revision_pack":
      return "Revision pack";
  }
}
