import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import {
  ValidatorPanel,
  ValidationFinding,
} from "../components/bim/ValidatorPanel";

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
        sourcePath="demo://project.ifc"
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
        sourcePath="demo://project.ifc"
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
        sourcePath="demo://project.ifc"
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
        sourcePath="demo://project.ifc"
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
});
