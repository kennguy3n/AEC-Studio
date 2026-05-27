/**
 * Smoke tests for the Deliver mode page.
 *
 * The page wires four components (toolbar, pack composer, export
 * targets, revision manager) against the in-process aec.deliver
 * fixture. These tests exercise the full create → list → compare flow
 * and the pack-build flow.
 */

import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

import { aec } from "../api/aec";
import { Deliver } from "../pages/Deliver";
import { ActiveProjectProvider } from "../hooks/useActiveProject";
import { ToastProvider } from "../hooks/useToast";

function renderDeliver() {
  return render(
    <ToastProvider>
      <ActiveProjectProvider>
        <Deliver />
      </ActiveProjectProvider>
    </ToastProvider>,
  );
}

describe("<Deliver />", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders pack composer, export targets, and revision manager", () => {
    renderDeliver();
    expect(screen.getByTestId("deliver-mode")).toBeInTheDocument();
    expect(screen.getByTestId("pack-composer")).toBeInTheDocument();
    expect(screen.getByTestId("export-target-list")).toBeInTheDocument();
    expect(screen.getByTestId("revision-manager")).toBeInTheDocument();
    expect(screen.getByTestId("deliver-toolbar")).toBeInTheDocument();
  });

  it("seeds default deliverables for concept pack on first mount", () => {
    renderDeliver();
    const rendersBox = screen
      .getByTestId("pack-deliverable-renders")
      .querySelector("input") as HTMLInputElement;
    const ifcBox = screen
      .getByTestId("pack-deliverable-ifc")
      .querySelector("input") as HTMLInputElement;
    // concept pack: renders on, IFC off+disabled (not applicable).
    expect(rendersBox.checked).toBe(true);
    expect(ifcBox.disabled).toBe(true);
  });

  it("switches deliverables when pack kind changes", () => {
    renderDeliver();
    const contractorRadio = screen
      .getByTestId("pack-kind-contractor")
      .querySelector("input") as HTMLInputElement;
    fireEvent.click(contractorRadio);
    const ifcBox = screen
      .getByTestId("pack-deliverable-ifc")
      .querySelector("input") as HTMLInputElement;
    expect(ifcBox.disabled).toBe(false);
    expect(ifcBox.checked).toBe(true);
  });

  it("creates a revision, then lists it", async () => {
    renderDeliver();
    fireEvent.change(screen.getByTestId("revision-tag-input"), {
      target: { value: "v1" },
    });
    fireEvent.change(screen.getByTestId("revision-description-input"), {
      target: { value: "first cut" },
    });
    fireEvent.click(screen.getByTestId("revision-create-button"));
    await waitFor(() => {
      expect(screen.queryByTestId("revision-empty")).not.toBeInTheDocument();
    });
    // The revision entry id is dynamic; look for the strong tag text.
    expect(screen.getByText("v1")).toBeInTheDocument();
    expect(screen.getByText("first cut")).toBeInTheDocument();
  });

  it("disables compare until two distinct revisions are picked", async () => {
    renderDeliver();
    const compareBtn = screen.getByTestId(
      "revision-compare-button",
    ) as HTMLButtonElement;
    expect(compareBtn.disabled).toBe(true);

    // Create two revisions.
    for (const tag of ["v1", "v2"]) {
      fireEvent.change(screen.getByTestId("revision-tag-input"), {
        target: { value: tag },
      });
      fireEvent.click(screen.getByTestId("revision-create-button"));
      await waitFor(() => {
        expect(screen.getByText(tag)).toBeInTheDocument();
      });
    }

    // Pick base = v1, head = v2 by clicking the per-row buttons.
    const baseButtons = screen.getAllByText(/Set as base/);
    fireEvent.click(baseButtons[0]);
    const headButtons = screen.getAllByText(/Set as head/);
    fireEvent.click(headButtons[headButtons.length - 1]);

    await waitFor(() => {
      expect(
        (screen.getByTestId("revision-compare-button") as HTMLButtonElement)
          .disabled,
      ).toBe(false);
    });

    fireEvent.click(screen.getByTestId("revision-compare-button"));
    await waitFor(() => {
      expect(screen.getByTestId("revision-diff-summary")).toBeInTheDocument();
    });
  });

  it("builds a pack and renders the resulting file list", async () => {
    // The Deliver page now opens a save dialog before building.
    // Mock it to return a chosen path.
    vi.spyOn(aec.dialog, "saveFile").mockResolvedValue({
      canceled: false,
      path: "/tmp/test-pack.zip",
    });
    renderDeliver();
    fireEvent.click(screen.getByTestId("pack-build"));
    await waitFor(() => {
      expect(screen.getByTestId("deliver-export-result")).toBeInTheDocument();
    });
    // Concept pack ships a manifest in our fixture.
    expect(screen.getByTestId("pack-file-manifest.json")).toBeInTheDocument();
  });
});

/**
 * The Deliver page reads the active project's
 * `KChatConfig::default_thread_id` from `kchat:status.defaultThreadId`
 * (surfaced through the IPC layer) and forwards it to
 * `<KChatReviewPanel threadId={…} />`. These tests pin the wiring:
 *
 * - when the bridge reports a per-project thread, the panel must
 *   key its ingest poll off *that* thread, and
 * - when the bridge reports `null` (no project open / project
 *   manifest omitted `default_thread_id`), the panel must fall back
 *   to the `"kchat-default"` constant which matches
 *   `aec_core::DEFAULT_THREAD_ID` on the publisher side.
 *
 * Both tests force the status mock into the *connected* path so the
 * panel's offline branch doesn't hide the thread-id heading.
 */
describe("<Deliver /> KChat thread wiring", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("forwards the active project's defaultThreadId to the review panel", async () => {
    vi.spyOn(aec.kchat, "status").mockResolvedValue({
      state: "connected",
      publisherKind: "local_ipc",
      instanceJson: null,
      defaultThreadId: "thread-from-project",
    });
    const ingestSpy = vi
      .spyOn(aec.kchat, "ingestReviews")
      .mockResolvedValue({
        threadId: "thread-from-project",
        commentsJson: "[]",
        cardsJson: "[]",
      });

    renderDeliver();
    await waitFor(() => {
      expect(
        screen.getByText("Reviews · thread-from-project"),
      ).toBeInTheDocument();
    });
    await waitFor(() => {
      expect(ingestSpy).toHaveBeenCalledWith(
        expect.objectContaining({ threadId: "thread-from-project" }),
      );
    });
  });

  it("falls back to 'kchat-default' when no project thread is set", async () => {
    vi.spyOn(aec.kchat, "status").mockResolvedValue({
      state: "connected",
      publisherKind: "local_ipc",
      instanceJson: null,
      defaultThreadId: null,
    });
    const ingestSpy = vi
      .spyOn(aec.kchat, "ingestReviews")
      .mockResolvedValue({
        threadId: "kchat-default",
        commentsJson: "[]",
        cardsJson: "[]",
      });

    renderDeliver();
    await waitFor(() => {
      expect(
        screen.getByText("Reviews · kchat-default"),
      ).toBeInTheDocument();
    });
    await waitFor(() => {
      expect(ingestSpy).toHaveBeenCalledWith(
        expect.objectContaining({ threadId: "kchat-default" }),
      );
    });
  });
});
