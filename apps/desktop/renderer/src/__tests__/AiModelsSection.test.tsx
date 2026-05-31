/**
 * Phase 18 Group A Task 3 — `AiModelsSection` covers the three
 * Ternary-Bonsai tier rows + the download / activation buttons. The
 * IPC layer is faked via `vi.spyOn(aec.ai, ...)` so the test can
 * drive the deterministic "downloading → verifying → completed"
 * progress state machine without spinning up the real bridge.
 */
import {
  fireEvent,
  render,
  screen,
  waitFor,
  act,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { AiModelsSection } from "../components/AiModelsSection";
import { aec } from "../api/aec";

function tierInfo(
  tier: "small" | "medium" | "large",
  available: boolean,
  sizeBytes: number,
) {
  return {
    tier,
    name: `Ternary-Bonsai ${tier} (1.58-bit GGUF Q2_0)`,
    filename: `Ternary-Bonsai-${tier}-Q2_0.gguf`,
    sizeBytes,
    available,
    sizeOnDisk: available ? sizeBytes : 0,
  };
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("AiModelsSection", () => {
  it("renders all three Ternary-Bonsai tiers", async () => {
    render(<AiModelsSection />);
    await waitFor(() => {
      expect(
        screen.getByTestId("settings-ai-model-small"),
      ).toBeInTheDocument();
      expect(
        screen.getByTestId("settings-ai-model-medium"),
      ).toBeInTheDocument();
      expect(
        screen.getByTestId("settings-ai-model-large"),
      ).toBeInTheDocument();
    });
  });

  it("calls aec.ai.downloadModel and surfaces progress + completion", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });

    // Resolve a long-running download promise manually so the
    // progress polling effect runs in between.
    let resolveDownload: (() => void) | null = null;
    const downloadSpy = vi
      .spyOn(aec.ai, "downloadModel")
      .mockImplementation(() => {
        return new Promise((resolve) => {
          resolveDownload = () =>
            resolve({
              tier: "small",
              path: "/tmp/Ternary-Bonsai-1.7B-Q2_0.gguf",
              sizeBytes: 463_290_464,
            });
        });
      });

    const progressStates = [
      {
        tier: "small" as const,
        downloaded: 100_000_000,
        total: 463_290_464,
        state: "downloading" as const,
        message: null,
      },
      {
        tier: "small" as const,
        downloaded: 463_290_464,
        total: 463_290_464,
        state: "verifying" as const,
        message: null,
      },
      {
        tier: "small" as const,
        downloaded: 463_290_464,
        total: 463_290_464,
        state: "completed" as const,
        message: null,
      },
    ];
    let progressIdx = 0;
    vi.spyOn(aec.ai, "downloadProgress").mockImplementation(async () => {
      const p = progressStates[Math.min(progressIdx, progressStates.length - 1)];
      progressIdx++;
      return p;
    });

    let availabilityCalls = 0;
    vi.spyOn(aec.ai, "modelAvailability").mockImplementation(async () => {
      availabilityCalls++;
      const smallAvailable = availabilityCalls > 1;
      return {
        tiers: [
          tierInfo("small", smallAvailable, 463_290_464),
          tierInfo("medium", false, 1_074_969_344),
          tierInfo("large", false, 2_182_184_672),
        ],
        activeTier: "small",
        modelsDir: "/var/models",
      };
    });

    render(<AiModelsSection />);

    const downloadBtn = await screen.findByTestId(
      "settings-ai-model-small-download",
    );
    fireEvent.click(downloadBtn);
    expect(downloadSpy).toHaveBeenCalledWith("small");

    // Drive the 500ms poll a few ticks so progressIdx advances.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });

    // After "completed" tick, the promise resolves and availability
    // refresh runs.
    resolveDownload!();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    await waitFor(() => {
      expect(
        screen.getByTestId("settings-ai-model-small-available"),
      ).toBeInTheDocument();
    });
  });

  it("calls setActiveTier when 'Set active' clicked on a downloaded non-active tier", async () => {
    const setActiveSpy = vi
      .spyOn(aec.ai, "setActiveTier")
      .mockResolvedValue(undefined);
    vi.spyOn(aec.ai, "modelAvailability").mockResolvedValue({
      tiers: [
        tierInfo("small", true, 463_290_464),
        tierInfo("medium", true, 1_074_969_344),
        tierInfo("large", false, 2_182_184_672),
      ],
      activeTier: "small",
      modelsDir: "/var/models",
    });
    render(<AiModelsSection />);
    const activateMedium = await screen.findByTestId(
      "settings-ai-model-medium-activate",
    );
    fireEvent.click(activateMedium);
    await waitFor(() => {
      expect(setActiveSpy).toHaveBeenCalledWith("medium");
    });
  });
});
