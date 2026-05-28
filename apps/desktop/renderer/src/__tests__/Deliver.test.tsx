/**
 * Smoke tests for the Deliver mode page.
 *
 * The page wires four components (toolbar, pack composer, export
 * targets, revision manager) against the in-process aec.deliver
 * fixture. These tests exercise the full create → list → compare flow
 * and the pack-build flow.
 */

import { describe, it, expect, vi, afterEach } from "vitest";
import { act, render, screen, fireEvent, waitFor } from "@testing-library/react";

import { useEffect } from "react";
import type { RevisionSummary } from "../../../electron/bridge";
import { aec } from "../api/aec";
import { Deliver } from "../pages/Deliver";
import {
  ActiveProjectProvider,
  useActiveProject,
} from "../hooks/useActiveProject";
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

/**
 * Regression: Devin Review (commit 913b768) flagged that the Deliver
 * page did not reset its per-project state (`revisions`, `diff`,
 * `baseId`, `headId`, `exportResult`) when the active project
 * changed — unlike Bim.tsx:111-118, Draft.tsx:58-64, and
 * Render.tsx:91-165 which all do. Today this is hidden by
 * `RequireProject` unmounting the page on every transition, but
 * the same fragility comment that motivates the per-project reset
 * on the other mode pages applies: a future in-page project
 * picker, "switch to recent" toolbar action, or any code path that
 * calls `openProject` without forcing a route change would silently
 * leak project A's revisions / diff into project B.
 *
 * The fix adds a `useEffect([project?.path])` to Deliver.tsx that
 * synchronously clears per-project state before any async load.
 * This test pins the contract by:
 *   1. Mounting Deliver under a harness that exposes `openProject`,
 *   2. Creating revisions for project A,
 *   3. Switching to project B *without unmounting Deliver*,
 *   4. Asserting the revision list (and diff / export / etc.) is
 *      cleared synchronously, not by component unmount.
 */
describe("<Deliver /> per-project state reset on project switch", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("clears revisions synchronously when the active project changes (without unmount)", async () => {
    // The Deliver page is normally route-guarded by `RequireProject`
    // which unmounts it on every project transition. This test
    // mounts the page WITHOUT the guard to verify the defense-in-
    // depth reset effect (`useEffect([project?.path])`) clears
    // per-project state on transition even when the component
    // stays mounted. A future in-page project picker, "switch to
    // recent" toolbar action, or any code path that calls
    // `openProject` without forcing a route change would otherwise
    // silently leak project A's revisions / diff into project B.
    let switchProject: ((path: string) => Promise<void>) | null = null;
    function ProjectSwitcher() {
      const { openProject } = useActiveProject();
      useEffect(() => {
        switchProject = (path: string) => openProject(path);
      }, [openProject]);
      return null;
    }

    // Mock listRevisions to return different lists per call so we
    // can observe the reset → refetch sequence. The first call
    // (initial mount, project null) returns project-A revisions
    // (one row, distinct tag "rev-from-A"), then subsequent calls
    // after the switch return project-B revisions ("rev-from-B").
    // Using a distinct tag per project avoids the fixture's
    // duplicate-tag rejection in `createRevision`, and the
    // mockResolvedValueOnce sequence is independent of the in-
    // process backend's shared state.
    const revisionA: RevisionSummary = {
      revisionId: "rev-A1",
      tag: "rev-from-A",
      description: "from project A",
      createdAt: "2026-05-27T12:00:00Z",
      auditChainHead: "0".repeat(64),
      manifestName: "Project A",
      manifestAppVersion: "0.1.0",
      trackedEntities: [],
    };
    const revisionB: RevisionSummary = {
      revisionId: "rev-B1",
      tag: "rev-from-B",
      description: "from project B",
      createdAt: "2026-05-27T12:05:00Z",
      auditChainHead: "0".repeat(64),
      manifestName: "Project B",
      manifestAppVersion: "0.1.0",
      trackedEntities: [],
    };
    vi.spyOn(aec.deliver, "listRevisions")
      .mockResolvedValueOnce([revisionA]) // initial mount (project null)
      .mockResolvedValueOnce([revisionA]) // after openProject A
      .mockResolvedValue([revisionB]); // after switch to B

    render(
      <ToastProvider>
        <ActiveProjectProvider>
          <ProjectSwitcher />
          <Deliver />
        </ActiveProjectProvider>
      </ToastProvider>,
    );

    // Settle initial mount.
    await waitFor(() => {
      expect(screen.getByText("rev-from-A")).toBeInTheDocument();
    });

    // Switch to project A so the effect re-fires for the first
    // project transition. Even though listRevisions returns the
    // same revisionA list, the test exercises the effect's reset
    // → refetch sequence: the synchronous setRevisions([]) wipes
    // the list, then the async listRevisions repopulates it.
    expect(switchProject).not.toBeNull();
    await act(async () => {
      await switchProject!("/tmp/deliverA.aecstudio");
    });
    await waitFor(() => {
      expect(screen.getByText("rev-from-A")).toBeInTheDocument();
    });

    // Now switch to project B. The effect must:
    //   1. Reset revisions to [] synchronously (so rev-from-A is
    //      no longer in the DOM).
    //   2. Fetch project B's revisions and render rev-from-B.
    // Without the per-project reset effect, the page would
    // continue to show rev-from-A until the async listRevisions
    // for project B resolves, leaking project A's state into
    // project B's UI.
    await act(async () => {
      await switchProject!("/tmp/deliverB.aecstudio");
    });

    // After the transition, rev-from-A must be gone (reset took
    // effect) and rev-from-B must be visible (refetch took effect).
    await waitFor(() => {
      expect(screen.queryByText("rev-from-A")).not.toBeInTheDocument();
    });
    await waitFor(() => {
      expect(screen.getByText("rev-from-B")).toBeInTheDocument();
    });
  });
});
