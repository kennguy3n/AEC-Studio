/**
 * Phase 18 Group C Task 16 — `ImageGenPanel` test suite.
 *
 * Mirrors `AiModelsSection.test.tsx`: the IPC layer is faked via
 * `vi.spyOn(aec.imageGen, ...)` so the test exercises every UI
 * branch — empty-descriptor wizard hint, missing-file download
 * button, progress polling, completion, failure routing to per-tier
 * banner (skipping the top-level error), and a successful
 * `generate` round-trip that re-renders the produced PNG.
 */
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ImageGenPanel,
  __imageGenPanelTestables,
} from "../components/ImageGenPanel";
import { aec } from "../api/aec";

const {
  normalizeImageGenDimension,
  clampImageGenSteps,
  clampImageGenCfg,
  IMAGE_GEN_DIM_MIN,
  IMAGE_GEN_DIM_MAX,
  IMAGE_GEN_STEPS_MIN,
  IMAGE_GEN_STEPS_MAX,
  IMAGE_GEN_CFG_MIN,
  IMAGE_GEN_CFG_MAX,
} = __imageGenPanelTestables;

const SIZE = 463_290_464;
const PIXEL_BASE64 =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

function availability(opts?: {
  available?: boolean;
  hasDescriptor?: boolean;
  modelsDir?: string;
}) {
  const hasDesc = opts?.hasDescriptor ?? true;
  const av = opts?.available ?? false;
  return {
    filename: hasDesc ? "sd-v1-5-q4_0.gguf" : "",
    sizeBytes: hasDesc ? SIZE : 0,
    available: av,
    sizeOnDisk: av ? SIZE : 0,
    downloadUrl: hasDesc
      ? "https://huggingface.co/example/sd-v1-5/resolve/main/sd-v1-5-q4_0.gguf"
      : null,
    blake3Hex: hasDesc
      ? "0000000000000000000000000000000000000000000000000000000000000000"
      : "",
    modelsDir: opts?.modelsDir ?? "/var/models/image_gen",
  };
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("ImageGenPanel form-field normalization helpers", () => {
  // Devin Review round 2 INFO: client-side form previously allowed
  // non-multiple-of-8 dimensions + out-of-range steps / cfg to
  // round-trip to the bridge, which then rejected the request after
  // the cold spawn. The Rust-side validator at
  // `crates/aec_bridge/src/service.rs::image_gen_generate` is still
  // the authoritative gate, but mirroring it here lets the user see
  // normalized values *before* paying the spawn cost. These tests
  // pin the lockstep between the JS-side helpers and the documented
  // Rust-side constraints.
  it("normalizes dimensions to nearest multiple of 8 within [64, 2048]", () => {
    expect(normalizeImageGenDimension(512)).toBe(512);
    // Snap to nearest multiple of 8: 515 → 512, 519 → 520.
    expect(normalizeImageGenDimension(515)).toBe(512);
    expect(normalizeImageGenDimension(519)).toBe(520);
    // Clamp below MIN: 1 → 64, 0 → 64, negative → 64.
    expect(normalizeImageGenDimension(0)).toBe(IMAGE_GEN_DIM_MIN);
    expect(normalizeImageGenDimension(1)).toBe(IMAGE_GEN_DIM_MIN);
    expect(normalizeImageGenDimension(-100)).toBe(IMAGE_GEN_DIM_MIN);
    // Clamp above MAX: 9999 → 2048.
    expect(normalizeImageGenDimension(9999)).toBe(IMAGE_GEN_DIM_MAX);
    // NaN / Infinity → MIN (never propagated as-is).
    expect(normalizeImageGenDimension(Number.NaN)).toBe(IMAGE_GEN_DIM_MIN);
    expect(normalizeImageGenDimension(Number.POSITIVE_INFINITY)).toBe(
      IMAGE_GEN_DIM_MAX,
    );
  });

  it("clamps steps to integer in [1, 150] mirroring the Rust validator", () => {
    expect(clampImageGenSteps(20)).toBe(20);
    expect(clampImageGenSteps(0)).toBe(IMAGE_GEN_STEPS_MIN);
    expect(clampImageGenSteps(-5)).toBe(IMAGE_GEN_STEPS_MIN);
    expect(clampImageGenSteps(500)).toBe(IMAGE_GEN_STEPS_MAX);
    // Non-integer input rounds.
    expect(clampImageGenSteps(20.4)).toBe(20);
    expect(clampImageGenSteps(20.6)).toBe(21);
    expect(clampImageGenSteps(Number.NaN)).toBe(IMAGE_GEN_STEPS_MIN);
  });

  it("clamps cfg_scale to [0.0, 30.0] preserving fractional precision", () => {
    expect(clampImageGenCfg(7)).toBe(7);
    expect(clampImageGenCfg(7.5)).toBe(7.5);
    expect(clampImageGenCfg(-1)).toBe(IMAGE_GEN_CFG_MIN);
    expect(clampImageGenCfg(100)).toBe(IMAGE_GEN_CFG_MAX);
    expect(clampImageGenCfg(Number.NaN)).toBe(IMAGE_GEN_CFG_MIN);
    expect(clampImageGenCfg(Number.POSITIVE_INFINITY)).toBe(IMAGE_GEN_CFG_MAX);
  });
});

describe("ImageGenPanel", () => {
  it("renders the wizard hint when no descriptor is configured", async () => {
    vi.spyOn(aec.imageGen, "modelAvailability").mockResolvedValue(
      availability({ hasDescriptor: false }),
    );
    vi.spyOn(aec.imageGen, "runtimeStatus").mockResolvedValue({
      state: "idle",
      lastError: null,
    });

    render(<ImageGenPanel />);
    await waitFor(() => {
      expect(
        screen.getByTestId("settings-image-gen-no-descriptor"),
      ).toBeInTheDocument();
    });
    // Download button must NOT be present when no descriptor is
    // configured — the user can only progress via the wizard.
    expect(
      screen.queryByTestId("settings-image-gen-download"),
    ).not.toBeInTheDocument();
  });

  it("shows the Download button when the model file is missing", async () => {
    vi.spyOn(aec.imageGen, "modelAvailability").mockResolvedValue(
      availability({ available: false, hasDescriptor: true }),
    );
    vi.spyOn(aec.imageGen, "runtimeStatus").mockResolvedValue({
      state: "idle",
      lastError: null,
    });

    render(<ImageGenPanel />);
    await waitFor(() => {
      expect(
        screen.getByTestId("settings-image-gen-download"),
      ).toBeInTheDocument();
    });
    expect(
      screen.getByTestId("settings-image-gen-missing"),
    ).toBeInTheDocument();
  });

  it("calls aec.imageGen.downloadModel and surfaces progress + completion", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });

    let resolveDownload: (() => void) | null = null;
    const downloadSpy = vi
      .spyOn(aec.imageGen, "downloadModel")
      .mockImplementation(() => {
        return new Promise((resolve) => {
          resolveDownload = () =>
            resolve({
              filename: "sd-v1-5-q4_0.gguf",
              path: "/var/models/image_gen/sd-v1-5-q4_0.gguf",
              sizeBytes: SIZE,
            });
        });
      });

    const progressStates = [
      {
        filename: "sd-v1-5-q4_0.gguf",
        downloaded: 100_000_000,
        total: SIZE,
        state: "downloading" as const,
        message: null,
      },
      {
        filename: "sd-v1-5-q4_0.gguf",
        downloaded: SIZE,
        total: SIZE,
        state: "verifying" as const,
        message: null,
      },
      {
        filename: "sd-v1-5-q4_0.gguf",
        downloaded: SIZE,
        total: SIZE,
        state: "completed" as const,
        message: null,
      },
    ];
    let progressIdx = 0;
    vi.spyOn(aec.imageGen, "downloadProgress").mockImplementation(async () => {
      const p = progressStates[Math.min(progressIdx, progressStates.length - 1)];
      progressIdx++;
      return p;
    });
    vi.spyOn(aec.imageGen, "runtimeStatus").mockResolvedValue({
      state: "idle",
      lastError: null,
    });

    let availabilityCalls = 0;
    vi.spyOn(aec.imageGen, "modelAvailability").mockImplementation(async () => {
      availabilityCalls++;
      return availability({ available: availabilityCalls > 1 });
    });

    render(<ImageGenPanel />);

    const downloadBtn = await screen.findByTestId(
      "settings-image-gen-download",
    );
    fireEvent.click(downloadBtn);
    expect(downloadSpy).toHaveBeenCalled();

    // Drive a few 500ms poll ticks so progressIdx advances.
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });

    resolveDownload!();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    await waitFor(() => {
      expect(
        screen.getByTestId("settings-image-gen-available"),
      ).toBeInTheDocument();
    });
  });

  it("routes a download failure to the per-tier banner only, not the top-level error", async () => {
    vi.spyOn(aec.imageGen, "downloadModel").mockRejectedValue(
      new Error("HTTP 503 from huggingface.co"),
    );
    vi.spyOn(aec.imageGen, "downloadProgress").mockResolvedValue({
      filename: "sd-v1-5-q4_0.gguf",
      downloaded: 12_345,
      total: SIZE,
      state: "failed",
      message: "HTTP 503 from huggingface.co",
    });
    vi.spyOn(aec.imageGen, "modelAvailability").mockResolvedValue(
      availability({ available: false }),
    );
    vi.spyOn(aec.imageGen, "runtimeStatus").mockResolvedValue({
      state: "idle",
      lastError: null,
    });

    render(<ImageGenPanel />);
    const downloadBtn = await screen.findByTestId(
      "settings-image-gen-download",
    );
    fireEvent.click(downloadBtn);
    await waitFor(() => {
      expect(
        screen.getByTestId("settings-image-gen-failed"),
      ).toBeInTheDocument();
    });
    // The dual-banner avoidance contract: when the per-tier banner
    // is showing, the top-level `settings-image-gen-error` must not
    // also be in the DOM for the same failure.
    expect(
      screen.queryByTestId("settings-image-gen-error"),
    ).not.toBeInTheDocument();
  });

  it("dismisses the failure banner and suppresses re-surfacing from the poll", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.spyOn(aec.imageGen, "downloadModel").mockRejectedValue(
      new Error("boom"),
    );
    vi.spyOn(aec.imageGen, "downloadProgress").mockResolvedValue({
      filename: "sd-v1-5-q4_0.gguf",
      downloaded: 0,
      total: SIZE,
      state: "failed",
      message: "boom",
    });
    vi.spyOn(aec.imageGen, "modelAvailability").mockResolvedValue(
      availability({ available: false }),
    );
    vi.spyOn(aec.imageGen, "runtimeStatus").mockResolvedValue({
      state: "idle",
      lastError: null,
    });

    render(<ImageGenPanel />);
    const downloadBtn = await screen.findByTestId(
      "settings-image-gen-download",
    );
    fireEvent.click(downloadBtn);
    await waitFor(() => {
      expect(
        screen.getByTestId("settings-image-gen-failed"),
      ).toBeInTheDocument();
    });

    // Click "Dismiss". The polling effect must not re-surface the
    // banner after dismissal.
    const dismiss = screen.getByText("Dismiss");
    fireEvent.click(dismiss);
    await waitFor(() => {
      expect(
        screen.queryByTestId("settings-image-gen-failed"),
      ).not.toBeInTheDocument();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_000);
    });
    expect(
      screen.queryByTestId("settings-image-gen-failed"),
    ).not.toBeInTheDocument();
  });

  it("generates an image and renders the resulting PNG + meta", async () => {
    vi.spyOn(aec.imageGen, "modelAvailability").mockResolvedValue(
      availability({ available: true }),
    );
    vi.spyOn(aec.imageGen, "runtimeStatus").mockResolvedValue({
      state: "ready",
      lastError: null,
    });
    const generateSpy = vi
      .spyOn(aec.imageGen, "generate")
      .mockResolvedValue({
        pngBase64: PIXEL_BASE64,
        seed: 42,
        width: 512,
        height: 512,
        steps: 20,
        info: "cfg=7.0",
      });

    render(<ImageGenPanel />);
    const prompt = await screen.findByTestId("settings-image-gen-prompt");
    fireEvent.change(prompt, { target: { value: "a renaissance cathedral" } });
    const btn = await screen.findByTestId("settings-image-gen-generate");
    expect(btn).not.toBeDisabled();
    fireEvent.click(btn);

    await waitFor(() => {
      expect(
        screen.getByTestId("settings-image-gen-result"),
      ).toBeInTheDocument();
    });
    expect(generateSpy).toHaveBeenCalledWith(
      expect.objectContaining({ prompt: "a renaissance cathedral" }),
    );
    const meta = screen.getByTestId("settings-image-gen-result-meta");
    expect(meta.textContent).toContain("512×512");
    expect(meta.textContent).toContain("seed 42");
    // The seed field is now pinned to the value the sidecar reported.
    expect(
      (screen.getByTestId("settings-image-gen-seed") as HTMLInputElement)
        .value,
    ).toBe("42");
  });

  it("shows a clear error when generate() rejects", async () => {
    vi.spyOn(aec.imageGen, "modelAvailability").mockResolvedValue(
      availability({ available: true }),
    );
    vi.spyOn(aec.imageGen, "runtimeStatus").mockResolvedValue({
      state: "failed",
      lastError: "sidecar exited",
    });
    vi.spyOn(aec.imageGen, "generate").mockRejectedValue(
      new Error("sd-server exited with status 1"),
    );

    render(<ImageGenPanel />);
    const prompt = await screen.findByTestId("settings-image-gen-prompt");
    fireEvent.change(prompt, { target: { value: "a renaissance cathedral" } });
    fireEvent.click(screen.getByTestId("settings-image-gen-generate"));
    await waitFor(() => {
      expect(screen.getByTestId("settings-image-gen-error")).toHaveTextContent(
        "sd-server exited with status 1",
      );
    });
  });

  it("rejects empty prompts without calling generate", async () => {
    vi.spyOn(aec.imageGen, "modelAvailability").mockResolvedValue(
      availability({ available: true }),
    );
    vi.spyOn(aec.imageGen, "runtimeStatus").mockResolvedValue({
      state: "idle",
      lastError: null,
    });
    const generateSpy = vi.spyOn(aec.imageGen, "generate");

    render(<ImageGenPanel />);
    // The button starts disabled when prompt is empty.
    const btn = await screen.findByTestId("settings-image-gen-generate");
    expect(btn).toBeDisabled();
    expect(generateSpy).not.toHaveBeenCalled();
  });

  // ---- Phase 18 Group C Task 17 — render-gate UX ----

  it("renders the paused banner + disables Generate when render is in progress on Low/Medium tier", async () => {
    vi.spyOn(aec.imageGen, "modelAvailability").mockResolvedValue(
      availability({ available: true }),
    );
    vi.spyOn(aec.imageGen, "runtimeStatus").mockResolvedValue({
      state: "ready",
      lastError: null,
    });
    // Medium-tier policy: gate is on.
    vi.spyOn(aec.imageGen, "activePolicy").mockResolvedValue({
      idleTimeoutSecs: 120,
      loadBudgetSecs: 60,
      maxParallelRequests: 1,
      allowDuringPathtracedRender: false,
    });
    vi.spyOn(aec.render, "pathtracedInProgress").mockResolvedValue(true);

    render(<ImageGenPanel />);
    const prompt = await screen.findByTestId("settings-image-gen-prompt");
    fireEvent.change(prompt, { target: { value: "a study with bookshelves" } });

    await waitFor(() => {
      expect(
        screen.getByTestId("settings-image-gen-gated"),
      ).toBeInTheDocument();
    });
    const btn = screen.getByTestId(
      "settings-image-gen-generate",
    ) as HTMLButtonElement;
    expect(btn).toBeDisabled();
    expect(btn.textContent).toContain("Paused");
  });

  it("does NOT render the paused banner on High/Pro tiers even while a render runs", async () => {
    vi.spyOn(aec.imageGen, "modelAvailability").mockResolvedValue(
      availability({ available: true }),
    );
    vi.spyOn(aec.imageGen, "runtimeStatus").mockResolvedValue({
      state: "ready",
      lastError: null,
    });
    // High-tier policy: gate is off.
    vi.spyOn(aec.imageGen, "activePolicy").mockResolvedValue({
      idleTimeoutSecs: 180,
      loadBudgetSecs: 75,
      maxParallelRequests: 2,
      allowDuringPathtracedRender: true,
    });
    vi.spyOn(aec.render, "pathtracedInProgress").mockResolvedValue(true);

    render(<ImageGenPanel />);
    const prompt = await screen.findByTestId("settings-image-gen-prompt");
    fireEvent.change(prompt, { target: { value: "a study with bookshelves" } });

    await waitFor(() => {
      const btn = screen.getByTestId(
        "settings-image-gen-generate",
      ) as HTMLButtonElement;
      expect(btn).not.toBeDisabled();
    });
    expect(
      screen.queryByTestId("settings-image-gen-gated"),
    ).not.toBeInTheDocument();
  });

  it("does NOT render the paused banner on Low tier when no render is running", async () => {
    vi.spyOn(aec.imageGen, "modelAvailability").mockResolvedValue(
      availability({ available: true }),
    );
    vi.spyOn(aec.imageGen, "runtimeStatus").mockResolvedValue({
      state: "ready",
      lastError: null,
    });
    // Low-tier policy: gate would activate IF a render were running.
    vi.spyOn(aec.imageGen, "activePolicy").mockResolvedValue({
      idleTimeoutSecs: 60,
      loadBudgetSecs: 45,
      maxParallelRequests: 1,
      allowDuringPathtracedRender: false,
    });
    vi.spyOn(aec.render, "pathtracedInProgress").mockResolvedValue(false);

    render(<ImageGenPanel />);
    const prompt = await screen.findByTestId("settings-image-gen-prompt");
    fireEvent.change(prompt, { target: { value: "a study with bookshelves" } });

    await waitFor(() => {
      const btn = screen.getByTestId(
        "settings-image-gen-generate",
      ) as HTMLButtonElement;
      expect(btn).not.toBeDisabled();
    });
    expect(
      screen.queryByTestId("settings-image-gen-gated"),
    ).not.toBeInTheDocument();
  });
});
