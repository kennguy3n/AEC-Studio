import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ScheduleView, ScheduleRow } from "../components/bim/ScheduleView";
import { aec } from "../api/aec";

const baseProps = {
  sourcePath: "/test/project.ifc",
  outPathForKind: (kind: string) => `/test/project.${kind}.xlsx`,
};

describe("ScheduleView", () => {
  it("renders all four schedule tabs and an empty body when no rows", () => {
    render(
      <ScheduleView
        {...baseProps}
        rowsByKind={{}}
        onGenerate={() => undefined}
      />,
    );
    expect(screen.getByTestId("schedule-tab-room")).toBeInTheDocument();
    expect(screen.getByTestId("schedule-tab-door")).toBeInTheDocument();
    expect(screen.getByTestId("schedule-tab-window")).toBeInTheDocument();
    expect(screen.getByTestId("schedule-tab-material")).toBeInTheDocument();
    expect(screen.getByTestId("schedule-empty")).toBeInTheDocument();
  });

  it("renders the rows table for the active tab", () => {
    const rows: ScheduleRow[] = [
      { number: "101", name: "Living", area: 23.5 },
      { number: "102", name: "Bedroom", area: 14.2 },
    ];
    render(
      <ScheduleView
        {...baseProps}
        rowsByKind={{ room: rows }}
        onGenerate={() => undefined}
      />,
    );
    const body = screen.getByTestId("schedule-rows-room");
    expect(body.querySelectorAll("tr").length).toBe(2);
  });

  it("renders columns in the writer-supplied header order, not the row's key order", () => {
    // Regression: the napi layer transports rows as
    // `HashMap<String, String>` so `Object.keys(rows[0])` is
    // non-deterministic across runs / V8 builds for non-integer
    // string keys. The writer's `header` is the source of truth
    // for column order, so the table headers must match
    // `headersByKind`, not the row's `Object.keys` ordering.
    const rows: ScheduleRow[] = [
      // Intentionally insert keys in a different order than the
      // writer's header to prove the component prefers the header.
      { area: 23.5, name: "Living", number: "101" },
      { area: 14.2, name: "Bedroom", number: "102" },
    ];
    render(
      <ScheduleView
        {...baseProps}
        rowsByKind={{ room: rows }}
        headersByKind={{ room: ["number", "name", "area"] }}
        onGenerate={() => undefined}
      />,
    );
    const headers = Array.from(
      screen
        .getByTestId("schedule-view")
        .querySelectorAll("thead th"),
    ).map((th) => th.textContent);
    expect(headers).toEqual(["number", "name", "area"]);
  });

  it("switches to door tab when clicked", () => {
    render(
      <ScheduleView
        {...baseProps}
        rowsByKind={{}}
        onGenerate={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("schedule-tab-door"));
    expect(
      screen.getByTestId("schedule-tab-door").getAttribute("aria-selected"),
    ).toBe("true");
  });

  it("calls the regenerate IPC and forwards a summary to onGenerate", async () => {
    const onGenerate = vi.fn();
    render(
      <ScheduleView
        {...baseProps}
        rowsByKind={{}}
        onGenerate={onGenerate}
      />,
    );
    fireEvent.click(screen.getByTestId("schedule-regenerate"));
    await waitFor(() => expect(onGenerate).toHaveBeenCalled());
    // First arg is the kind discriminator the bridge was asked
    // to generate for; second is the summary object surfaced to
    // the parent (scheduleId / outPath / row+column counts).
    const [kind, summary] = onGenerate.mock.calls[0];
    expect(kind).toBe("room");
    expect(typeof summary.scheduleId).toBe("string");
    expect(summary.outPath).toBe("/test/project.room.xlsx");
  });

  it("disables the Regenerate button when no IFC is imported (empty sourcePath)", () => {
    const onGenerate = vi.fn();
    render(
      <ScheduleView
        sourcePath=""
        outPathForKind={baseProps.outPathForKind}
        rowsByKind={{}}
        onGenerate={onGenerate}
      />,
    );
    const btn = screen.getByTestId("schedule-regenerate") as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    // Clicking a disabled button must not invoke the parent callback;
    // without the empty-string guard the bridge would receive an empty
    // path and fail on the native side.
    fireEvent.click(btn);
    expect(onGenerate).not.toHaveBeenCalled();
  });

  // Defense-in-depth regression: Devin Review flagged that the
  // `regenerate` callback itself lacks an internal empty-`sourcePath`
  // guard. Today the button-disabled check (`disabled={busy ||
  // sourcePath === ""}`) makes the bug unreachable from the UI, but
  // a future non-button caller (parent-driven re-generation on mount,
  // a keyboard shortcut, a "regenerate all" toolbar action) could
  // invoke the function programmatically with an empty path. The
  // internal `if (busy || sourcePath === "") return;` guard at the
  // top of `regenerate` makes that programmatic path safe too.
  // We simulate the future caller by force-removing the button's
  // disabled attribute and firing a click — without the internal
  // guard the IPC spy would record a call with `sourcePath: ""`.
  it("regenerate() internal guard skips the IPC when sourcePath is empty", async () => {
    const onGenerate = vi.fn();
    const generateSpy = vi.spyOn(aec.bim, "generateSchedule");
    render(
      <ScheduleView
        sourcePath=""
        outPathForKind={baseProps.outPathForKind}
        rowsByKind={{}}
        onGenerate={onGenerate}
      />,
    );
    const btn = screen.getByTestId("schedule-regenerate") as HTMLButtonElement;
    // Simulate a future caller that bypasses the disabled-button UX
    // (e.g. wires the click handler to a keyboard shortcut and forgets
    // to mirror the disabled check). The internal guard must hold.
    btn.removeAttribute("disabled");
    fireEvent.click(btn);
    // Give any unguarded promise a tick to settle so a missed guard
    // would surface as a spy invocation before this assertion runs.
    await Promise.resolve();
    expect(generateSpy).not.toHaveBeenCalled();
    expect(onGenerate).not.toHaveBeenCalled();
    generateSpy.mockRestore();
  });

  // Regression test: the `regenerate` callback's `try/finally` /
  // `catch` covers bridge failures from real OS paths supplied by
  // the file picker. Disk-full / permission-denied / locked-DB
  // errors would otherwise become silent unhandled promise
  // rejections. The fix routes bridge failures through the optional
  // `onError` callback so the parent (Bim.tsx) can surface them
  // via `addToast("error", ...)`.
  it("regenerate() surfaces bridge failures via onError", async () => {
    const onGenerate = vi.fn();
    const onError = vi.fn();
    const generateSpy = vi
      .spyOn(aec.bim, "generateSchedule")
      .mockRejectedValueOnce(new Error("disk full"));
    render(
      <ScheduleView
        {...baseProps}
        rowsByKind={{}}
        onGenerate={onGenerate}
        onError={onError}
      />,
    );
    fireEvent.click(screen.getByTestId("schedule-regenerate"));
    await waitFor(() => expect(onError).toHaveBeenCalledTimes(1));
    // The display message must include the bridge error so the user
    // can act on it (retry / free disk / pick a different out-path).
    expect(onError.mock.calls[0][0]).toMatch(
      /Regenerate room schedule failed: disk full/,
    );
    // onGenerate must NOT fire on failure — the XLSX wasn't written.
    expect(onGenerate).not.toHaveBeenCalled();
    // The button must re-enable for retry (finally block clears busy).
    const btn = screen.getByTestId(
      "schedule-regenerate",
    ) as HTMLButtonElement;
    await waitFor(() => expect(btn.disabled).toBe(false));
    generateSpy.mockRestore();
  });

  // Companion regression: when `onError` is omitted (isolated unit
  // tests, hypothetical future callers that forget to wire it), the
  // panel falls back to `console.error` so the failure is still
  // observable in dev rather than silently swallowed. The button
  // must still re-enable for retry.
  it("regenerate() logs to console when onError is omitted", async () => {
    const consoleSpy = vi
      .spyOn(console, "error")
      .mockImplementation(() => undefined);
    const generateSpy = vi
      .spyOn(aec.bim, "generateSchedule")
      .mockRejectedValueOnce(new Error("permission denied"));
    render(
      <ScheduleView
        {...baseProps}
        rowsByKind={{}}
        onGenerate={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("schedule-regenerate"));
    await waitFor(() => expect(consoleSpy).toHaveBeenCalled());
    const message = consoleSpy.mock.calls[0][0] as string;
    expect(message).toMatch(
      /Regenerate room schedule failed: permission denied/,
    );
    const btn = screen.getByTestId(
      "schedule-regenerate",
    ) as HTMLButtonElement;
    await waitFor(() => expect(btn.disabled).toBe(false));
    generateSpy.mockRestore();
    consoleSpy.mockRestore();
  });
});
