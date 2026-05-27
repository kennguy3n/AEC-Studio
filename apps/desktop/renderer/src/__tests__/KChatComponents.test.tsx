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
    });
    const reload = vi.spyOn(aec.kchat, "reload").mockResolvedValue({
      state: "connected",
      publisherKind: "local_ipc",
      instanceJson: JSON.stringify({
        socket_path: "/tmp/kchat.sock",
        version: "2.0.0",
        health: "ok",
      }),
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
});
