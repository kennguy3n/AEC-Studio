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

  it("renders one row per suggestion", () => {
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
    // The in-process backend returns suggestions: [] by default.
    await waitFor(() => expect(onSuggestions).toHaveBeenCalled());
    expect(onSuggestions).toHaveBeenCalledWith([]);
  });
});
