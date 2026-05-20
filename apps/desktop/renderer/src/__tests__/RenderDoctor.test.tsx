import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import {
  RenderDoctor,
  DoctorSuggestion,
} from "../components/render/RenderDoctor";

const SUGGESTIONS: DoctorSuggestion[] = [
  {
    code: "NOISY_INTERIOR",
    severity: "warning",
    message: "Interior shot is noisy; try more samples.",
    fix: "Raise samples to 256",
  },
];

const MATERIAL_SUGGESTIONS: DoctorSuggestion[] = [
  {
    code: "material.missing_texture",
    severity: "error",
    materialId: "mat:oak",
    message: "mat:oak: albedo texture blob `deadbeef` is missing",
    fix: "Re-link the missing texture or re-import the asset",
  },
  {
    code: "material.non_pbr",
    severity: "warning",
    materialId: "mat:gold",
    message: "mat:gold: non-PBR — metallic 1.4 out of [0,1]",
    fix: null,
  },
];

describe("RenderDoctor", () => {
  it("disables Diagnose without a job id", () => {
    render(
      <RenderDoctor
        jobId={null}
        suggestions={[]}
        onSuggestions={() => undefined}
      />,
    );
    expect(
      (screen.getByTestId("render-doctor-diagnose") as HTMLButtonElement)
        .disabled,
    ).toBe(true);
    expect(screen.getByTestId("render-doctor-empty")).toBeInTheDocument();
  });

  it("renders one row per render-time suggestion", () => {
    render(
      <RenderDoctor
        jobId="job_001"
        suggestions={SUGGESTIONS}
        onSuggestions={() => undefined}
      />,
    );
    expect(screen.getByTestId("render-doctor-item-0").textContent).toContain(
      "NOISY_INTERIOR",
    );
  });

  it("invokes diagnose IPC and forwards suggestions", async () => {
    const onSuggestions = vi.fn();
    render(
      <RenderDoctor
        jobId="job_001"
        suggestions={[]}
        onSuggestions={onSuggestions}
      />,
    );
    fireEvent.click(screen.getByTestId("render-doctor-diagnose"));
    await waitFor(() => expect(onSuggestions).toHaveBeenCalled());
    expect(onSuggestions).toHaveBeenCalledWith([]);
  });

  it("groups material findings under their own header", () => {
    render(
      <RenderDoctor
        jobId="job_001"
        suggestions={MATERIAL_SUGGESTIONS}
        onSuggestions={() => undefined}
      />,
    );
    expect(
      screen.getByTestId("render-doctor-group-material"),
    ).toBeInTheDocument();
    expect(
      screen.getByTestId("render-doctor-material-0").textContent,
    ).toContain("mat:oak");
    expect(
      screen.getByTestId("render-doctor-material-1").textContent,
    ).toContain("mat:gold");
    // No render-time rows when only material findings are supplied.
    expect(screen.queryByTestId("render-doctor-item-0")).not.toBeInTheDocument();
  });

  it("invokes onCheckMaterials and forwards returned suggestions", async () => {
    const onSuggestions = vi.fn();
    const onCheckMaterials = vi.fn().mockResolvedValue(MATERIAL_SUGGESTIONS);
    render(
      <RenderDoctor
        jobId={null}
        suggestions={[]}
        onSuggestions={onSuggestions}
        onCheckMaterials={onCheckMaterials}
      />,
    );
    fireEvent.click(screen.getByTestId("render-doctor-check-materials"));
    await waitFor(() => expect(onSuggestions).toHaveBeenCalled());
    expect(onSuggestions).toHaveBeenCalledWith(MATERIAL_SUGGESTIONS);
  });

  it("hides Check Materials button when no handler supplied", () => {
    render(
      <RenderDoctor
        jobId={null}
        suggestions={[]}
        onSuggestions={() => undefined}
      />,
    );
    expect(
      screen.queryByTestId("render-doctor-check-materials"),
    ).not.toBeInTheDocument();
  });
});
