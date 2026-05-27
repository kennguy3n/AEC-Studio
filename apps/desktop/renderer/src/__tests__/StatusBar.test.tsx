/**
 * StatusBar render-job-polling regression tests.
 *
 * Devin Review flagged that the original StatusBar polled
 * `aec.render.listJobs()` every 5s unconditionally, even when no
 * project was active (Home screen, initial route hit before a
 * project is opened). Render jobs are always scoped to a project, so
 * the count is structurally zero whenever `project === null` — every
 * such IPC round-trip was pure waste (burns battery, log spam, and
 * blocks the bridge worker for a result the UI cannot use).
 *
 * The fix splits the static hardware/AI status fetch (one-shot on
 * mount) from the render-queue poller (gated on `project !== null`).
 * The render poller also resets the count to 0 on transitions so a
 * stale count from the previous project can't bleed into the new
 * project's status line for up to one poll interval. These tests
 * pin the project-aware gating + reset contract.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen } from "@testing-library/react";
import { aec } from "../api/aec";
import { ActiveProjectProvider, useActiveProject } from "../hooks/useActiveProject";
import { StatusBar } from "../components/StatusBar";

function OpenAndCloseHarness({ pathToOpen }: { pathToOpen: string }) {
  const { openProject, closeProject } = useActiveProject();
  return (
    <div>
      <StatusBar />
      <button
        type="button"
        data-testid="open"
        onClick={() => {
          void openProject(pathToOpen);
        }}
      >
        open
      </button>
      <button
        type="button"
        data-testid="close"
        onClick={() => {
          void closeProject();
        }}
      >
        close
      </button>
    </div>
  );
}

describe("StatusBar — render job polling gates on active project", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("does not poll aec.render.listJobs when no project is active", async () => {
    // No project mounted (provider's mount-effect resolves to null).
    // Even after multiple 5s polling intervals, the spy must stay at
    // zero — Home-screen / initial-route IPC traffic for render
    // queue must be exactly zero.
    const listJobsSpy = vi.spyOn(aec.render, "listJobs");

    render(
      <ActiveProjectProvider>
        <StatusBar />
      </ActiveProjectProvider>,
    );

    // Let the provider's `refreshProject` settle to project=null.
    await act(async () => {
      await Promise.resolve();
    });

    // Advance through several poll intervals.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(30_000);
    });

    expect(listJobsSpy).not.toHaveBeenCalled();
    listJobsSpy.mockRestore();
  });

  it("starts polling aec.render.listJobs once a project becomes active", async () => {
    const listJobsSpy = vi
      .spyOn(aec.render, "listJobs")
      .mockResolvedValue([]);

    render(
      <ActiveProjectProvider>
        <OpenAndCloseHarness pathToOpen="/tmp/StatusBarPoll.aecstudio" />
      </ActiveProjectProvider>,
    );

    await act(async () => {
      await Promise.resolve();
    });

    expect(listJobsSpy).not.toHaveBeenCalled();

    // Open a project — the project-keyed effect must re-run with the
    // new project and dispatch the initial poll tick immediately,
    // followed by interval ticks every 5s.
    await act(async () => {
      screen.getByTestId("open").click();
    });
    // Let the open's bridge round-trip resolve and the effect re-run.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    // Initial tick on project transition.
    expect(listJobsSpy).toHaveBeenCalledTimes(1);

    // Two more interval ticks at +5s and +10s.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(listJobsSpy).toHaveBeenCalledTimes(2);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(listJobsSpy).toHaveBeenCalledTimes(3);

    listJobsSpy.mockRestore();
  });

  it("stops polling and resets the render-count display when the project closes", async () => {
    // Mock listJobs to return an active job so the count chip surfaces;
    // after close it must disappear (count resets to 0), proving both
    // the cleanup ran AND the count was reset to 0 to prevent stale
    // bleed-through into a future project.
    const listJobsSpy = vi
      .spyOn(aec.render, "listJobs")
      .mockResolvedValue([{ jobId: "j1", status: "running" }]);

    render(
      <ActiveProjectProvider>
        <OpenAndCloseHarness pathToOpen="/tmp/StatusBarClose.aecstudio" />
      </ActiveProjectProvider>,
    );

    await act(async () => {
      await Promise.resolve();
    });

    // Open project → polling starts → count chip surfaces.
    await act(async () => {
      screen.getByTestId("open").click();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    expect(screen.queryByTestId("status-render-count")).not.toBeNull();
    const callsAfterOpen = listJobsSpy.mock.calls.length;
    expect(callsAfterOpen).toBeGreaterThan(0);

    // Close → polling stops + count resets to 0 → chip disappears.
    await act(async () => {
      screen.getByTestId("close").click();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    // Advance two more poll windows — the spy must NOT pick up any
    // more calls (interval was cleared by the effect cleanup).
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000);
    });
    expect(listJobsSpy.mock.calls.length).toBe(callsAfterOpen);

    // Count chip is gone (renderJobCount reset to 0, condition
    // `renderJobCount > 0` no longer matches).
    expect(screen.queryByTestId("status-render-count")).toBeNull();

    listJobsSpy.mockRestore();
  });

  it("does NOT restart the render-job poller when the active project's modifiedAt changes (e.g. on save)", async () => {
    // Devin Review flagged that listing `project` (the whole summary
    // object) in the polling effect's deps causes the interval to
    // tear down and re-create on every save, because
    // `useActiveProject` re-creates the summary reference on each
    // save to surface the refreshed `modifiedAt` to consumers. The
    // fix is to key the effect on `project?.path` — the stable
    // identity across saves. This test pins that behaviour by
    // asserting the interval tick count is unchanged across N saves
    // of the *same* path: the existing interval must keep firing,
    // not be replaced by a fresh interval per save (which would
    // additionally fire its own initial tick every time).
    const listJobsSpy = vi
      .spyOn(aec.render, "listJobs")
      .mockResolvedValue([]);

    function SaveHarness() {
      const { openProject, saveProject } = useActiveProject();
      return (
        <div>
          <StatusBar />
          <button
            type="button"
            data-testid="open"
            onClick={() => {
              void openProject("/tmp/StatusBarSave.aecstudio");
            }}
          >
            open
          </button>
          <button
            type="button"
            data-testid="save"
            onClick={() => {
              void saveProject();
            }}
          >
            save
          </button>
        </div>
      );
    }

    render(
      <ActiveProjectProvider>
        <SaveHarness />
      </ActiveProjectProvider>,
    );

    await act(async () => {
      await Promise.resolve();
    });

    await act(async () => {
      screen.getByTestId("open").click();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });

    // Initial tick on project open.
    const callsAfterOpen = listJobsSpy.mock.calls.length;
    expect(callsAfterOpen).toBeGreaterThanOrEqual(1);

    // Issue several saves in rapid succession. If the effect were
    // keyed on `project` (the object), each save would tear down +
    // recreate the interval and fire a fresh initial tick, so the
    // call count would jump by ~5 immediately. With the `project?.
    // path` keying, the saves are no-ops for the polling effect: no
    // extra ticks happen until the next 5s window.
    for (let i = 0; i < 5; i++) {
      await act(async () => {
        screen.getByTestId("save").click();
      });
      await act(async () => {
        await vi.advanceTimersByTimeAsync(0);
      });
    }

    expect(listJobsSpy.mock.calls.length).toBe(callsAfterOpen);

    // The existing interval must still fire on the next 5s boundary
    // — proving it was not torn down and replaced.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(listJobsSpy.mock.calls.length).toBe(callsAfterOpen + 1);

    listJobsSpy.mockRestore();
  });

  it("returns a cleanup function on every branch of the render-job effect (close→open→close→open does not leak intervals)", async () => {
    // Devin Review (commit be262cd) flagged that the no-project
    // early-return branch fell through `return;` (implicit undefined)
    // rather than returning a cleanup closure. React tolerates a
    // bare return — `undefined` is interpreted as "no cleanup needed"
    // — but the asymmetric contract is a footgun: a future
    // contributor adding any async work above the `return;` would
    // silently leave it running after unmount because the no-project
    // branch never runs the cleanup-emission path. The fix returns
    // `() => {}` from the early branch.
    //
    // This test pins the symmetric-cleanup contract by cycling the
    // active project null → A → null → B and asserting that the
    // total interval count is bounded — if cleanup ever leaked an
    // interval (because the no-project branch's return value tripped
    // up React's deps-changed teardown sequence), the spy would
    // accumulate calls from BOTH the closed-project's stale interval
    // AND the newly-mounted project's interval.
    const listJobsSpy = vi
      .spyOn(aec.render, "listJobs")
      .mockResolvedValue([]);

    function CycleHarness() {
      const { openProject, closeProject } = useActiveProject();
      return (
        <div>
          <StatusBar />
          <button
            type="button"
            data-testid="open-A"
            onClick={() => {
              void openProject("/tmp/StatusBarCycleA.aecstudio");
            }}
          >
            open A
          </button>
          <button
            type="button"
            data-testid="open-B"
            onClick={() => {
              void openProject("/tmp/StatusBarCycleB.aecstudio");
            }}
          >
            open B
          </button>
          <button
            type="button"
            data-testid="close"
            onClick={() => {
              void closeProject();
            }}
          >
            close
          </button>
        </div>
      );
    }

    render(
      <ActiveProjectProvider>
        <CycleHarness />
      </ActiveProjectProvider>,
    );

    await act(async () => {
      await Promise.resolve();
    });

    // null → A: one initial tick.
    await act(async () => {
      screen.getByTestId("open-A").click();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const afterOpenA = listJobsSpy.mock.calls.length;
    expect(afterOpenA).toBeGreaterThan(0);

    // A → null: cleanup runs, count resets, no new ticks for 15s.
    await act(async () => {
      screen.getByTestId("close").click();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(15_000);
    });
    expect(listJobsSpy.mock.calls.length).toBe(afterOpenA);

    // null → B: the no-project branch's `return () => {}` cleanup
    // ran cleanly on the previous transition, so this open mounts a
    // fresh interval with exactly one initial tick — no leaked
    // interval from the prior closed state.
    await act(async () => {
      screen.getByTestId("open-B").click();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    const afterOpenB = listJobsSpy.mock.calls.length;
    expect(afterOpenB).toBe(afterOpenA + 1);

    // One more 5s window → exactly one more tick (no leaked interval
    // doubling up the cadence).
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });
    expect(listJobsSpy.mock.calls.length).toBe(afterOpenB + 1);

    listJobsSpy.mockRestore();
  });
});
