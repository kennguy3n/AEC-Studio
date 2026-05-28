import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import {
  ValidatorPanel,
  ValidationFinding,
} from "../components/bim/ValidatorPanel";
import { aec } from "../api/aec";

const FINDINGS: ValidationFinding[] = [
  {
    code: "MISSING_CLASS",
    severity: "warning",
    message: "Element has no classification",
    entityId: "ent_001",
  },
  {
    code: "DUPLICATE_GUID",
    severity: "error",
    message: "Two entities share GUID X",
    entityId: "ent_002",
  },
];

describe("ValidatorPanel", () => {
  it("shows the empty state when no findings", () => {
    render(
      <ValidatorPanel
        sourcePath="/test/project.ifc"
        findings={[]}
        onFindings={() => undefined}
        onZoomTo={() => undefined}
      />,
    );
    expect(screen.getByTestId("validator-empty")).toBeInTheDocument();
    expect(screen.getByTestId("validator-count").textContent).toBe(
      "0 finding(s)",
    );
  });

  it("renders one row per finding with the severity tag", () => {
    render(
      <ValidatorPanel
        sourcePath="/test/project.ifc"
        findings={FINDINGS}
        onFindings={() => undefined}
        onZoomTo={() => undefined}
      />,
    );
    expect(screen.getByTestId("validator-count").textContent).toBe(
      "2 finding(s)",
    );
    expect(screen.getByTestId("validator-item-0").textContent).toContain(
      "MISSING_CLASS",
    );
    expect(screen.getByTestId("validator-item-1").textContent).toContain(
      "DUPLICATE_GUID",
    );
  });

  it("invokes onZoomTo with the entity id when a finding has one", () => {
    const onZoomTo = vi.fn();
    render(
      <ValidatorPanel
        sourcePath="/test/project.ifc"
        findings={FINDINGS}
        onFindings={() => undefined}
        onZoomTo={onZoomTo}
      />,
    );
    fireEvent.click(screen.getByTestId("validator-zoom-0"));
    expect(onZoomTo).toHaveBeenCalledWith("ent_001");
  });

  it("Re-validate hits the IPC and reports merged findings", async () => {
    const onFindings = vi.fn();
    render(
      <ValidatorPanel
        sourcePath="/test/project.ifc"
        findings={[]}
        onFindings={onFindings}
        onZoomTo={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("validator-revalidate"));
    await waitFor(() => expect(onFindings).toHaveBeenCalled());
    // The default in-process backend returns
    // `{ ok: true, sourcePath, schema: "IFC4", errors: [], warnings: [], infos: [], parseCacheHit: false }`,
    // which `bimReportToFindings` flattens to an empty `ValidationFinding[]`.
    expect(onFindings).toHaveBeenCalledWith([]);
  });

  // Regression test: Devin Review flagged that when no IFC has been
  // imported, the `sourcePath` is `""` and clicking Re-validate
  // would invoke `aec.bim.validate({ sourcePath: "" })` — the
  // production main-process IPC handler asserts the string is
  // non-empty (`assertString` rejects `""`) and the error becomes
  // an unhandled promise rejection with no user feedback. The
  // disabled-state guard makes the bug unreachable from the UI.
  it("disables Re-validate when sourcePath is empty (no IFC imported)", () => {
    render(
      <ValidatorPanel
        sourcePath=""
        findings={[]}
        onFindings={() => undefined}
        onZoomTo={() => undefined}
      />,
    );
    const button = screen.getByTestId(
      "validator-revalidate",
    ) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(button.title).toBe("Import an IFC first");
  });

  it("enables Re-validate once a sourcePath is provided", () => {
    render(
      <ValidatorPanel
        sourcePath="/test/project.ifc"
        findings={[]}
        onFindings={() => undefined}
        onZoomTo={() => undefined}
      />,
    );
    const button = screen.getByTestId(
      "validator-revalidate",
    ) as HTMLButtonElement;
    expect(button.disabled).toBe(false);
    expect(button.title).toBe("");
  });

  // Defense-in-depth regression: the `revalidate` callback itself
  // now has an internal empty-`sourcePath` guard, mirroring the
  // ScheduleView `regenerate` guard. Today the button-disabled
  // check makes the bug unreachable from the UI, but a future
  // non-button caller (parent-driven re-validation on mount, a
  // keyboard shortcut, a "validate all" toolbar action) could
  // invoke the function programmatically with an empty path. The
  // internal guard makes that programmatic path safe too.
  it("revalidate() internal guard skips the IPC when sourcePath is empty", async () => {
    const onFindings = vi.fn();
    const validateSpy = vi.spyOn(aec.bim, "validate");
    render(
      <ValidatorPanel
        sourcePath=""
        findings={[]}
        onFindings={onFindings}
        onZoomTo={() => undefined}
      />,
    );
    const btn = screen.getByTestId(
      "validator-revalidate",
    ) as HTMLButtonElement;
    // Simulate a future caller that bypasses the disabled-button UX
    // (e.g. wires the click handler to a keyboard shortcut and
    // forgets to mirror the disabled check). The internal guard
    // must hold.
    btn.removeAttribute("disabled");
    fireEvent.click(btn);
    await Promise.resolve();
    expect(validateSpy).not.toHaveBeenCalled();
    expect(onFindings).not.toHaveBeenCalled();
    validateSpy.mockRestore();
  });

  // Regression test: Devin Review flagged that the `revalidate`
  // callback had `try { ... } finally { ... }` with no `catch`.
  // Pre-Phase 13 every branch hit `demo://` paths (in-process
  // fallback never throws), so missing catch was benign; Phase 13
  // wires real OS paths from the file picker so file-moved /
  // permission-denied / malformed-IFC errors became silent
  // unhandled promise rejections. The fix routes bridge failures
  // through the optional `onError` callback so the parent (Bim.tsx)
  // can surface them via `addToast("error", ...)`.
  it("revalidate() surfaces bridge failures via onError", async () => {
    const onFindings = vi.fn();
    const onError = vi.fn();
    const validateSpy = vi
      .spyOn(aec.bim, "validate")
      .mockRejectedValueOnce(new Error("file not found"));
    render(
      <ValidatorPanel
        sourcePath="/test/project.ifc"
        findings={[]}
        onFindings={onFindings}
        onZoomTo={() => undefined}
        onError={onError}
      />,
    );
    fireEvent.click(screen.getByTestId("validator-revalidate"));
    await waitFor(() => expect(onError).toHaveBeenCalledTimes(1));
    expect(onError.mock.calls[0][0]).toMatch(
      /Re-validate failed: file not found/,
    );
    // onFindings must NOT fire on failure — no report was produced.
    expect(onFindings).not.toHaveBeenCalled();
    // The button must re-enable for retry (finally block clears busy).
    const btn = screen.getByTestId(
      "validator-revalidate",
    ) as HTMLButtonElement;
    await waitFor(() => expect(btn.disabled).toBe(false));
    validateSpy.mockRestore();
  });

  // Companion regression: when `onError` is omitted, the panel
  // falls back to `console.error` so the failure is still observable
  // in dev rather than silently swallowed. The button must still
  // re-enable for retry.
  it("revalidate() logs to console when onError is omitted", async () => {
    const consoleSpy = vi
      .spyOn(console, "error")
      .mockImplementation(() => undefined);
    const validateSpy = vi
      .spyOn(aec.bim, "validate")
      .mockRejectedValueOnce(new Error("locked DB"));
    render(
      <ValidatorPanel
        sourcePath="/test/project.ifc"
        findings={[]}
        onFindings={() => undefined}
        onZoomTo={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("validator-revalidate"));
    await waitFor(() => expect(consoleSpy).toHaveBeenCalled());
    const message = consoleSpy.mock.calls[0][0] as string;
    expect(message).toMatch(/Re-validate failed: locked DB/);
    const btn = screen.getByTestId(
      "validator-revalidate",
    ) as HTMLButtonElement;
    await waitFor(() => expect(btn.disabled).toBe(false));
    validateSpy.mockRestore();
    consoleSpy.mockRestore();
  });
});
