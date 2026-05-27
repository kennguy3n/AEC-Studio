import { describe, it, expect, vi, afterEach } from "vitest";
import {
  render,
  screen,
  fireEvent,
  waitFor,
  cleanup,
} from "@testing-library/react";

import { KChatStatusIndicator } from "../components/kchat/KChatStatusIndicator";
import { PublishCardModal } from "../components/kchat/PublishCardModal";
import { KChatReviewPanel } from "../components/kchat/KChatReviewPanel";
import { aec } from "../api/aec";

describe("KChatStatusIndicator", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it("renders the disconnected chip when the bridge reports in_memory", async () => {
    vi.spyOn(aec.kchat, "status").mockResolvedValue({
      state: "disconnected",
      publisherKind: "in_memory",
      instanceJson: null,
      defaultThreadId: null,
    });
    render(<KChatStatusIndicator />);
    await waitFor(() =>
      expect(screen.getByTestId("kchat-status-chip").textContent).toContain(
        "offline",
      ),
    );
    expect(
      screen.getByTestId("kchat-status-chip").getAttribute("data-state"),
    ).toBe("disconnected");
  });

  it("renders the connected chip with version tooltip", async () => {
    vi.spyOn(aec.kchat, "status").mockResolvedValue({
      state: "connected",
      publisherKind: "local_ipc",
      instanceJson: JSON.stringify({
        socket_path: "/tmp/kchat.sock",
        version: "1.4.2",
        health: "ok",
      }),
      defaultThreadId: null,
    });
    render(<KChatStatusIndicator />);
    await waitFor(() =>
      expect(screen.getByTestId("kchat-status-chip").textContent).toContain(
        "online",
      ),
    );
    expect(
      screen.getByTestId("kchat-status-chip").getAttribute("title"),
    ).toContain("1.4.2");
  });

  it("invokes reload when the chip is clicked", async () => {
    vi.spyOn(aec.kchat, "status").mockResolvedValue({
      state: "disconnected",
      publisherKind: "in_memory",
      instanceJson: null,
      defaultThreadId: null,
    });
    const reload = vi.spyOn(aec.kchat, "reload").mockResolvedValue({
      state: "connected",
      publisherKind: "local_ipc",
      instanceJson: JSON.stringify({
        socket_path: "/tmp/kchat.sock",
        version: "2.0.0",
        health: "ok",
      }),
      defaultThreadId: null,
    });
    render(<KChatStatusIndicator />);
    await waitFor(() =>
      expect(screen.getByTestId("kchat-status-chip").textContent).toContain(
        "offline",
      ),
    );
    fireEvent.click(screen.getByTestId("kchat-status-chip"));
    await waitFor(() => expect(reload).toHaveBeenCalledTimes(1));
  });

  // Regression test for Devin Review finding "onReload lacks catch
  // block, propagating unhandled promise rejection". `onReload`
  // must catch every IPC error so the React onClick handler doesn't
  // leak unhandled rejections (React ignores returned promises),
  // and must surface the failure to the user via the chip's title
  // / data-reload-error attributes rather than failing silently.
  it("captures reload failures into the chip tooltip and avoids unhandled rejections", async () => {
    let unhandled: unknown = null;
    const handler = (e: PromiseRejectionEvent) => {
      unhandled = e.reason;
    };
    window.addEventListener("unhandledrejection", handler);

    vi.spyOn(aec.kchat, "status").mockResolvedValue({
      state: "disconnected",
      publisherKind: "in_memory",
      instanceJson: null,
      defaultThreadId: null,
    });
    vi.spyOn(aec.kchat, "reload").mockRejectedValue(
      new Error("socket unreachable"),
    );

    render(<KChatStatusIndicator />);
    await waitFor(() =>
      expect(screen.getByTestId("kchat-status-chip").textContent).toContain(
        "offline",
      ),
    );
    fireEvent.click(screen.getByTestId("kchat-status-chip"));
    await waitFor(() => {
      const chip = screen.getByTestId("kchat-status-chip");
      expect(chip.getAttribute("data-reload-error")).toBe("socket unreachable");
      expect(chip.getAttribute("title")).toContain("socket unreachable");
      expect(chip.getAttribute("aria-label")).toContain("reload failed");
      // The `finally` must still re-enable the chip even on the
      // error branch — otherwise the user can never retry.
      expect((chip as HTMLButtonElement).disabled).toBe(false);
    });

    // No unhandled promise rejection escapes the React onClick.
    expect(unhandled).toBeNull();

    window.removeEventListener("unhandledrejection", handler);
  });
});

describe("PublishCardModal", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it("renders the artifact kind chip and disables submit on empty caption", () => {
    render(
      <PublishCardModal
        artifactKind="concept_render"
        projectLink="aecstudio://project/x/render/y"
        defaultCaption=""
        thumbnailBlake3={null}
        onClose={() => undefined}
      />,
    );
    expect(screen.getByTestId("kchat-publish-kind").textContent).toContain(
      "Concept render",
    );
    expect(
      (screen.getByTestId("kchat-publish-submit") as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });

  it("publishes via the bridge and reports the message_id", async () => {
    const publish = vi.spyOn(aec.kchat, "publish").mockResolvedValue({
      messageId: "msg-1",
      threadId: "kchat-default",
      publishedAt: "2026-05-27T00:00:00Z",
    });
    const onClose = vi.fn();
    render(
      <PublishCardModal
        artifactKind="sheet_set"
        projectLink="aecstudio://project/x/sheet/y"
        defaultCaption="ground-floor plans"
        thumbnailBlake3="blake3:abcd1234"
        onClose={onClose}
      />,
    );
    fireEvent.click(screen.getByTestId("kchat-publish-submit"));
    await waitFor(() => expect(publish).toHaveBeenCalledTimes(1));
    const cardJson = (publish.mock.calls[0][0] as { cardJson: string })
      .cardJson;
    const parsed = JSON.parse(cardJson) as {
      artifact: string;
      caption: string;
      thumbnail_blake3: string | null;
    };
    expect(parsed.artifact).toBe("sheet_set");
    expect(parsed.caption).toBe("ground-floor plans");
    expect(parsed.thumbnail_blake3).toBe("blake3:abcd1234");
    expect(onClose).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: "published",
        messageId: "msg-1",
        threadId: "kchat-default",
      }),
    );
  });

  it("surfaces transport errors via onClose(failed)", async () => {
    vi.spyOn(aec.kchat, "publish").mockRejectedValue(
      new Error("socket closed"),
    );
    const onClose = vi.fn();
    render(
      <PublishCardModal
        artifactKind="asset_pack"
        projectLink="aecstudio://project/x/pack/y"
        defaultCaption="pack v1"
        thumbnailBlake3={null}
        onClose={onClose}
      />,
    );
    fireEvent.click(screen.getByTestId("kchat-publish-submit"));
    await waitFor(() =>
      expect(onClose).toHaveBeenCalledWith(
        expect.objectContaining({ kind: "failed", message: "socket closed" }),
      ),
    );
  });
});

describe("KChatReviewPanel", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    cleanup();
  });

  it("renders the offline message when the bridge reports the in-memory fallback", async () => {
    vi.spyOn(aec.kchat, "status").mockResolvedValue({
      state: "disconnected",
      publisherKind: "in_memory",
      instanceJson: null,
      defaultThreadId: null,
    });
    render(<KChatReviewPanel threadId="kchat-default" />);
    const panel = await screen.findByTestId("kchat-review-panel");
    await waitFor(() =>
      expect(panel.getAttribute("data-offline")).toBe("true"),
    );
  });

  it("renders ingested comments when the bridge reports connected", async () => {
    vi.spyOn(aec.kchat, "status").mockResolvedValue({
      state: "connected",
      publisherKind: "local_ipc",
      instanceJson: JSON.stringify({
        socket_path: "/tmp/kchat.sock",
        version: "1.0.0",
        health: "ok",
      }),
      defaultThreadId: null,
    });
    vi.spyOn(aec.kchat, "ingestReviews").mockResolvedValue({
      threadId: "kchat-default",
      commentsJson: JSON.stringify([
        {
          comment_id: "c1",
          author: "alice",
          text: "Looks great!",
          posted_at: "2026-05-27T00:00:00Z",
          artifact_id: null,
        },
        {
          comment_id: "c2",
          author: "bob",
          text: "Move the window 12cm right",
          posted_at: "2026-05-27T00:05:00Z",
          artifact_id: "render-1",
        },
      ]),
      cardsJson: "[]",
    });
    render(<KChatReviewPanel threadId="kchat-default" />);
    const list = await screen.findByTestId("kchat-review-list");
    await waitFor(() =>
      expect(list.querySelectorAll("li").length).toBe(2),
    );
    expect(list.textContent).toContain("alice");
    expect(list.textContent).toContain("Move the window");
  });

  // Regression test for Devin Review finding "fetchOnce lacks catch
  // block, producing unhandled promise rejections on every poll
  // failure". `fetchOnce` must catch every IPC error so the two
  // `void fetchOnce()` fire-and-forget call sites don't leak
  // unhandled rejections, and must surface the failure inline so
  // the user can see *why* the panel is stale rather than the
  // panel silently presenting stale data.
  it("renders an inline error banner when the bridge throws and keeps prior comments visible", async () => {
    let unhandled: unknown = null;
    const handler = (e: PromiseRejectionEvent) => {
      unhandled = e.reason;
    };
    window.addEventListener("unhandledrejection", handler);

    vi.spyOn(aec.kchat, "status").mockResolvedValue({
      state: "connected",
      publisherKind: "local_ipc",
      instanceJson: JSON.stringify({
        socket_path: "/tmp/kchat.sock",
        version: "1.0.0",
        health: "ok",
      }),
      defaultThreadId: null,
    });
    vi.spyOn(aec.kchat, "ingestReviews").mockRejectedValue(
      new Error("bridge timed out"),
    );

    render(<KChatReviewPanel threadId="kchat-default" />);
    const banner = await screen.findByTestId("kchat-review-error");
    expect(banner.textContent).toContain("bridge timed out");
    // No unhandled promise rejection from the fire-and-forget
    // call sites.
    expect(unhandled).toBeNull();

    window.removeEventListener("unhandledrejection", handler);
  });

  // Regression test for Devin Review finding "panel retains stale
  // comments and sinceIso cursor when threadId prop changes". When
  // the parent re-renders the panel with a different `threadId`
  // (e.g. the active project's `KChatConfig::default_thread_id`
  // changes), the panel must:
  //   1. clear the previous thread's comments from the list, and
  //   2. reset the `sinceIso` cursor so the new thread starts at
  //      the bridge's natural head, not at the previous thread's
  //      newest-comment timestamp.
  //
  // We assert (1) by checking the rendered list, and (2) by
  // checking the `sinceIso` argument passed to the *second*
  // `ingestReviews` invocation after the prop flips — it must be
  // null again, not the timestamp the previous thread advanced it
  // to.
  it("resets comments and sinceIso cursor when threadId prop changes", async () => {
    vi.spyOn(aec.kchat, "status").mockResolvedValue({
      state: "connected",
      publisherKind: "local_ipc",
      instanceJson: JSON.stringify({
        socket_path: "/tmp/kchat.sock",
        version: "1.0.0",
        health: "ok",
      }),
      defaultThreadId: null,
    });
    const ingest = vi
      .spyOn(aec.kchat, "ingestReviews")
      .mockImplementation(async ({ threadId }) => {
        if (threadId === "thread-a") {
          return {
            threadId,
            commentsJson: JSON.stringify([
              {
                comment_id: "a1",
                author: "alice",
                text: "comment on thread A",
                posted_at: "2026-05-27T00:00:00Z",
                artifact_id: null,
              },
            ]),
            cardsJson: "[]",
          };
        }
        return {
          threadId,
          commentsJson: JSON.stringify([
            {
              comment_id: "b1",
              author: "bob",
              text: "comment on thread B",
              posted_at: "2026-05-27T00:01:00Z",
              artifact_id: null,
            },
          ]),
          cardsJson: "[]",
        };
      });

    const { rerender } = render(<KChatReviewPanel threadId="thread-a" />);
    await waitFor(() => {
      const list = screen.getByTestId("kchat-review-list");
      const items = list.querySelectorAll("li");
      expect(items.length).toBe(1);
      expect(list.textContent).toContain("alice");
    });

    // First ingest call: sinceIso must be null (initial cursor).
    expect(ingest.mock.calls[0][0]).toMatchObject({
      threadId: "thread-a",
      sinceIso: null,
    });

    // Capture the number of ingest calls observed for thread-a so
    // we can locate the first thread-b call cleanly without
    // depending on race timing.
    const callsAfterA = ingest.mock.calls.length;

    rerender(<KChatReviewPanel threadId="thread-b" />);

    // The first ingest call observed after the threadId flip must
    // address thread-b *and* must pass sinceIso=null — confirming
    // that the per-thread cursor was reset rather than leaking the
    // timestamp the previous thread had advanced it to. We check
    // this *first* (against the mock call log) before the DOM
    // assertion below, because the cursor-reset is the structural
    // invariant; the rendered list is a downstream consequence.
    await waitFor(() => {
      const firstBCall = ingest.mock.calls
        .slice(callsAfterA)
        .find(
          (args) =>
            (args[0] as { threadId: string }).threadId === "thread-b",
        );
      expect(firstBCall).toBeDefined();
      expect(firstBCall![0]).toMatchObject({
        threadId: "thread-b",
        sinceIso: null,
      });
    });

    // And the rendered list must show *only* thread B's comment —
    // the previous thread's "alice" row must not bleed across the
    // switch. We re-query the list each tick because the panel
    // briefly swaps the `<ul data-testid="kchat-review-list">` for
    // the empty-state `<p>` while `comments` is `[]` between the
    // reset effect and the next ingest resolving — a stale node
    // reference captured before the rerender would point at the
    // detached `<ul>` still holding "alice" and never observe the
    // recovered "bob" state.
    await waitFor(() => {
      const list = screen.getByTestId("kchat-review-list");
      const items = list.querySelectorAll("li");
      expect(items.length).toBe(1);
      expect(list.textContent).toContain("bob");
      expect(list.textContent).not.toContain("alice");
    });
  });

  // Regression test for Devin Review finding: an in-flight fetchOnce
  // from the previous thread resolving *after* a threadId prop change
  // must not corrupt the new thread's comment list or sinceIso cursor.
  // The fix uses a `threadIdRef` guard that discards stale responses.
  it("discards in-flight fetch results from a stale thread after threadId changes", async () => {
    // The status call always reports connected.
    vi.spyOn(aec.kchat, "status").mockResolvedValue({
      state: "connected",
      publisherKind: "local_ipc",
      instanceJson: JSON.stringify({
        socket_path: "/tmp/kchat.sock",
        version: "1.0.0",
        health: "ok",
      }),
      defaultThreadId: null,
    });

    // ingestReviews: thread-a resolves after a 100 ms delay (simulating
    // a slow network); thread-b resolves instantly.
    let resolveSlowA: (() => void) | null = null;
    const ingest = vi
      .spyOn(aec.kchat, "ingestReviews")
      .mockImplementation(async ({ threadId }) => {
        if (threadId === "thread-a") {
          await new Promise<void>((r) => {
            resolveSlowA = r;
          });
          return {
            threadId,
            commentsJson: JSON.stringify([
              {
                comment_id: "stale-a1",
                author: "alice",
                text: "stale from old thread",
                posted_at: "2026-05-27T00:00:00Z",
                artifact_id: null,
              },
            ]),
            cardsJson: "[]",
          };
        }
        return {
          threadId,
          commentsJson: JSON.stringify([
            {
              comment_id: "b1",
              author: "bob",
              text: "comment on thread B",
              posted_at: "2026-05-27T00:01:00Z",
              artifact_id: null,
            },
          ]),
          cardsJson: "[]",
        };
      });

    // Mount with thread-a — its fetch is now waiting on the deferred promise.
    const { rerender } = render(<KChatReviewPanel threadId="thread-a" />);
    // Wait for the status call at least.
    await waitFor(() => expect(ingest).toHaveBeenCalled());

    // Switch to thread-b *before* thread-a's ingest resolves.
    rerender(<KChatReviewPanel threadId="thread-b" />);

    // thread-b should resolve and render bob's comment.
    await waitFor(() => {
      const list = screen.getByTestId("kchat-review-list");
      expect(list.textContent).toContain("bob");
    });

    // Now let the stale thread-a response land.
    expect(resolveSlowA).not.toBeNull();
    resolveSlowA!();

    // Give the event loop a chance to process the resolved promise.
    await new Promise((r) => setTimeout(r, 50));

    // The stale "alice" comment must NOT appear — the threadIdRef
    // guard discarded the response.
    const list = screen.getByTestId("kchat-review-list");
    expect(list.textContent).not.toContain("alice");
    expect(list.textContent).toContain("bob");
    const items = list.querySelectorAll("li");
    expect(items.length).toBe(1);
  });
});
