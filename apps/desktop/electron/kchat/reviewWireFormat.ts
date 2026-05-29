/**
 * Wire-format mapping between the `.kcz` extension's review
 * comment payload and the renderer's `KChatReviewPanel` row
 * shape.
 *
 * Round-5 BUG_0001 fix-forward: the Phase 15 IPC handler
 * `kchat:ingestReviews` used to return `StoredReviewComment` shape
 * (`messageId` / `bodyMarkdown` / `postedAt`) directly, while the
 * renderer's `<KChatReviewPanel />` decodes `ReviewCommentRow`
 * (`comment_id` / `text` / `posted_at` / `artifact_id`). The two
 * shapes share zero field names; every cell rendered would have
 * been `undefined` and the cursor would never advance because the
 * dedup-by-id key was always `undefined`.
 *
 * Doing the rename here, at the IPC boundary, keeps the renderer
 * contract identical to Phase 12 so the existing dedup-by-id,
 * cursor advance, and React-key paths all keep working untouched.
 *
 * `artifact_id` is forced to `null` because the .kcz extension
 * does not (yet) propagate an originating artifact id back into
 * review comments — when it does, a real field will flow through
 * `ReviewCommentPayload` and be surfaced here.
 *
 * The function is kept in a standalone pure module (no electron /
 * node deps) so the renderer test suite can exercise it directly
 * with a real `ReviewCommentPayload[]` payload, closing the
 * ANALYSIS_0005 test-quality gap (existing mocks returned the
 * renderer shape, masking the BUG_0001 wire mismatch).
 */

import type { ReviewCommentPayload } from "./kchatLocalApi";

/**
 * Renderer-visible review row. MUST stay in lockstep with the
 * `ReviewCommentRow` type in
 * `apps/desktop/renderer/src/components/kchat/KChatReviewPanel.tsx`.
 */
export interface ReviewCommentRow {
  comment_id: string;
  author: string;
  text: string;
  posted_at: string;
  artifact_id: string | null;
}

/**
 * Map a chronological slice of `.kcz`-ingested review comments
 * (the shape `kchatAppState` accumulates from
 * `POST /api/review-comments`) to the renderer's
 * `ReviewCommentRow` shape.
 *
 * The mapping is total — every input produces exactly one output
 * row, in the same order. No filtering, no dedup; dedup belongs
 * upstream (`kchatAppState.getReviewCommentsForThread`) and
 * downstream (`mergeAndSort` in `KChatReviewPanel`).
 */
export function mapStoredReviewCommentsToRows(
  stored: readonly ReviewCommentPayload[],
): ReviewCommentRow[] {
  return stored.map((c) => ({
    comment_id: c.messageId,
    author: c.authorDisplayName,
    text: c.bodyMarkdown,
    posted_at: c.postedAt,
    artifact_id: null,
  }));
}
