/**
 * IPC-boundary wire-format test for `kchat:ingestReviews`.
 *
 * Closes the round-5 ANALYSIS_0005 test-quality gap: the existing
 * `KChatComponents.test.tsx` mocks return the renderer's
 * `ReviewCommentRow` shape directly, so they passed even with
 * BUG_0001 (the IPC handler returning `StoredReviewComment` /
 * `messageId` / `bodyMarkdown` / `postedAt` while the renderer
 * decoded `comment_id` / `text` / `posted_at`).
 *
 * This test feeds a real `ReviewCommentPayload[]` payload — the
 * shape the `.kcz` extension actually POSTs to
 * `/api/review-comments` — through the actual mapping helper the
 * IPC handler invokes, and asserts the result matches the
 * renderer's `ReviewCommentRow` shape field-for-field. If the
 * BUG_0001 mismatch ever regresses, this test catches it without
 * needing to spin up a full Electron + IPC stack.
 */

import { describe, expect, it } from "vitest";
import {
  mapStoredReviewCommentsToRows,
  type ReviewCommentRow,
} from "../../../electron/kchat/reviewWireFormat";
import type { ReviewCommentPayload } from "../../../electron/kchat/kchatLocalApi";

describe("mapStoredReviewCommentsToRows (kchat:ingestReviews wire format)", () => {
  it("maps every StoredReviewComment field onto the renderer's ReviewCommentRow shape", () => {
    // Payload exactly as the .kcz extension POSTs it to
    // `/api/review-comments` (see ReviewCommentsRequest in
    // kchatLocalApi.ts). No renderer-shaped fields here.
    const stored: ReviewCommentPayload[] = [
      {
        messageId: "kchat-msg-001",
        authorId: "@alex",
        authorDisplayName: "Alex Reviewer",
        bodyMarkdown: "Looks good — ship it.",
        postedAt: "2025-01-15T10:30:00.000Z",
        permalink: "https://kchat.example/teams/aec/channels/general/msg/001",
      },
      {
        messageId: "kchat-msg-002",
        authorId: "@jamie",
        authorDisplayName: "Jamie Lead",
        bodyMarkdown: "Approved with minor notes.",
        postedAt: "2025-01-15T11:00:00.000Z",
        permalink: null,
      },
    ];

    const rows = mapStoredReviewCommentsToRows(stored);

    expect(rows).toHaveLength(2);

    // Verify the wire transform. Every assertion here would fail
    // if BUG_0001 ever regresses (i.e. if the handler starts
    // returning the StoredReviewComment shape again).
    expect(rows[0]).toEqual<ReviewCommentRow>({
      comment_id: "kchat-msg-001",
      author: "Alex Reviewer",
      text: "Looks good — ship it.",
      posted_at: "2025-01-15T10:30:00.000Z",
      artifact_id: null,
    });
    expect(rows[1]).toEqual<ReviewCommentRow>({
      comment_id: "kchat-msg-002",
      author: "Jamie Lead",
      text: "Approved with minor notes.",
      posted_at: "2025-01-15T11:00:00.000Z",
      artifact_id: null,
    });
  });

  it("preserves input order — sorting is the renderer's responsibility", () => {
    // KChatReviewPanel calls `mergeAndSort` on every refresh; the
    // IPC handler must NOT re-sort because that would mask any
    // upstream ordering bug from `kchatAppState.ingestComments`.
    const stored: ReviewCommentPayload[] = [
      {
        messageId: "msg-c",
        authorId: "@a",
        authorDisplayName: "A",
        bodyMarkdown: "third",
        postedAt: "2025-01-15T12:00:00.000Z",
      },
      {
        messageId: "msg-a",
        authorId: "@a",
        authorDisplayName: "A",
        bodyMarkdown: "first",
        postedAt: "2025-01-15T10:00:00.000Z",
      },
      {
        messageId: "msg-b",
        authorId: "@a",
        authorDisplayName: "A",
        bodyMarkdown: "second",
        postedAt: "2025-01-15T11:00:00.000Z",
      },
    ];

    const rows = mapStoredReviewCommentsToRows(stored);

    expect(rows.map((r) => r.comment_id)).toEqual([
      "msg-c",
      "msg-a",
      "msg-b",
    ]);
  });

  it("produces a renderer payload the KChatReviewPanel decoder can ingest end-to-end", () => {
    // End-to-end shape check: JSON-encode the mapper output the
    // way the IPC handler does (`commentsJson: JSON.stringify(...)`)
    // and decode it back the way `parseComments` in
    // KChatReviewPanel does. Asserts the round-trip preserves
    // every renderer-visible field.
    const stored: ReviewCommentPayload[] = [
      {
        messageId: "round-trip-1",
        authorId: "@author",
        authorDisplayName: "Round Trip",
        bodyMarkdown: "Body with **markdown** and a [link](https://x).",
        postedAt: "2025-02-01T08:15:00.000Z",
      },
    ];

    const rows = mapStoredReviewCommentsToRows(stored);
    const wire = JSON.stringify(rows);
    const decoded = JSON.parse(wire) as ReviewCommentRow[];

    expect(decoded).toHaveLength(1);
    expect(decoded[0].comment_id).toBe("round-trip-1");
    expect(decoded[0].author).toBe("Round Trip");
    expect(decoded[0].text).toBe(
      "Body with **markdown** and a [link](https://x).",
    );
    expect(decoded[0].posted_at).toBe("2025-02-01T08:15:00.000Z");
    expect(decoded[0].artifact_id).toBeNull();
  });

  it("returns an empty array for an empty input (covers the boot-time / cold-thread case)", () => {
    expect(mapStoredReviewCommentsToRows([])).toEqual([]);
  });

  it("treats messageId as the dedup key the renderer will use", () => {
    // The renderer's `mergeAndSort` uses `comment_id` as the dedup
    // key. If the mapper ever stopped surfacing a stable
    // `comment_id`, every refresh would duplicate rows. Pin the
    // contract: distinct `messageId` ↔ distinct `comment_id`.
    const stored: ReviewCommentPayload[] = [
      {
        messageId: "uuid-1",
        authorId: "@a",
        authorDisplayName: "A",
        bodyMarkdown: "x",
        postedAt: "2025-01-15T10:00:00.000Z",
      },
      {
        messageId: "uuid-2",
        authorId: "@a",
        authorDisplayName: "A",
        bodyMarkdown: "y",
        postedAt: "2025-01-15T10:01:00.000Z",
      },
    ];

    const rows = mapStoredReviewCommentsToRows(stored);
    const ids = new Set(rows.map((r) => r.comment_id));
    expect(ids.size).toBe(2);
    expect(ids.has("uuid-1")).toBe(true);
    expect(ids.has("uuid-2")).toBe(true);
  });
});
