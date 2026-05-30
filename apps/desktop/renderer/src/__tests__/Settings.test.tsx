import { describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { Settings } from "../pages/Settings";
import { aec } from "../api/aec";

describe("Settings page", () => {
  it("renders all configuration sections", async () => {
    render(
      <MemoryRouter>
        <Settings />
      </MemoryRouter>,
    );

    expect(screen.getByTestId("settings-page")).toBeInTheDocument();
    expect(screen.getByTestId("settings-section-hardware")).toBeInTheDocument();
    expect(screen.getByTestId("settings-section-ai")).toBeInTheDocument();
    expect(screen.getByTestId("settings-section-render")).toBeInTheDocument();
    expect(screen.getByTestId("settings-section-region")).toBeInTheDocument();
    expect(screen.getByTestId("settings-section-kchat")).toBeInTheDocument();

    // Hardware tier resolves from the in-process runtime status backend.
    await waitFor(() => {
      const tier = screen.getByTestId("settings-hw-tier");
      expect(tier.textContent).not.toEqual("Detecting…");
    });
  });

  it("AI tier override defaults to auto and emits a change", () => {
    render(
      <MemoryRouter>
        <Settings />
      </MemoryRouter>,
    );
    const select = screen.getByTestId(
      "settings-ai-tier",
    ) as HTMLSelectElement;
    expect(select.value).toEqual("auto");
    fireEvent.change(select, { target: { value: "small" } });
    expect(select.value).toEqual("small");
  });

  it("KChat toggle hydrates from the bridge and writes through on flip", async () => {
    // Phase 15 (gate restoration): the Settings toggle is no
    // longer in-memory. It hydrates from `kchat:status.enabled`
    // (the bridge-persisted `KChatConfig::enabled`) and writes
    // through via `kchat:setEnabled`. The in-process renderer
    // backend returns `enabled: true` from status() to mirror
    // `KChatConfig::default()`.
    const setEnabledSpy = vi.spyOn(aec.kchat, "setEnabled");
    render(
      <MemoryRouter>
        <Settings />
      </MemoryRouter>,
    );
    const toggle = screen.getByTestId(
      "settings-kchat-enabled",
    ) as HTMLInputElement;
    // Wait for the initial `kchat:status` poll to land.
    await waitFor(() => {
      expect(toggle.checked).toBe(true);
    });
    fireEvent.click(toggle);
    await waitFor(() => {
      expect(setEnabledSpy).toHaveBeenCalledWith({ enabled: false });
    });
    await waitFor(() => {
      expect(toggle.checked).toBe(false);
    });
    fireEvent.click(toggle);
    await waitFor(() => {
      expect(setEnabledSpy).toHaveBeenLastCalledWith({ enabled: true });
    });
    await waitFor(() => {
      expect(toggle.checked).toBe(true);
    });
  });

  it("Save button records a saved timestamp", async () => {
    vi.useFakeTimers();
    render(
      <MemoryRouter>
        <Settings />
      </MemoryRouter>,
    );
    expect(
      screen.queryByTestId("settings-saved-stamp"),
    ).not.toBeInTheDocument();
    fireEvent.click(screen.getByTestId("settings-save"));
    expect(screen.getByTestId("settings-saved-stamp")).toBeInTheDocument();
    vi.useRealTimers();
  });

  it("hides the extension diagnostics card when there are no failures", async () => {
    // The renderer in-process backend returns an empty array from
    // `extensions.listLoadDiagnostics()`, which is the same signal
    // the production bridge sends on the no-broken-extensions path.
    render(
      <MemoryRouter>
        <Settings />
      </MemoryRouter>,
    );
    // Wait for the diagnostics fetch to complete so we know the
    // absence of the card is the no-failures path, not the
    // still-loading path. Anchoring on hardware tier resolution
    // (same useEffect cycle) keeps the assertion robust to any
    // future reordering of the effects.
    await waitFor(() => {
      const tier = screen.getByTestId("settings-hw-tier");
      expect(tier.textContent).not.toEqual("Detecting…");
    });
    expect(
      screen.queryByTestId("settings-section-extension-diagnostics"),
    ).not.toBeInTheDocument();
  });

  it("renders the extension diagnostics card when failures are present", async () => {
    // Spy on `extensions.listLoadDiagnostics` so we drive the
    // diagnostics card via the real Settings → aec API path
    // rather than reaching into component internals. The wire
    // shape mirrors `ExtensionLoadDiagnostic` in `bridge.ts`.
    const diagSpy = vi
      .spyOn(aec.extensions, "listLoadDiagnostics")
      .mockResolvedValueOnce([
        {
          extensionId: "demo.broken-assets",
          path: "/extensions/demo.broken-assets",
          stage: "asset_pack_install",
          message: "blake3 mismatch on cabinet.glb",
        },
        {
          extensionId: null,
          path: "/extensions/unparseable/manifest.json",
          stage: "manifest_parse",
          message: "expected `,` or `}` at line 17 column 3",
        },
      ]);
    render(
      <MemoryRouter>
        <Settings />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(
        screen.getByTestId("settings-section-extension-diagnostics"),
      ).toBeInTheDocument();
    });
    const items = screen.getAllByTestId("settings-extension-diagnostic-item");
    expect(items).toHaveLength(2);
    // First diagnostic: named extension, asset-pack-install stage.
    expect(items[0].getAttribute("data-extension-id")).toEqual(
      "demo.broken-assets",
    );
    expect(items[0].getAttribute("data-stage")).toEqual("asset_pack_install");
    expect(items[0].textContent).toContain("demo.broken-assets");
    expect(items[0].textContent).toContain("Asset-pack install failed");
    expect(items[0].textContent).toContain("blake3 mismatch on cabinet.glb");
    // Second diagnostic: unparseable manifest, no extension id.
    expect(items[1].getAttribute("data-extension-id")).toEqual("");
    expect(items[1].getAttribute("data-stage")).toEqual("manifest_parse");
    expect(items[1].textContent).toContain("(unknown extension)");
    expect(items[1].textContent).toContain("Manifest parse error");
    diagSpy.mockRestore();
  });

  it("region radios switch between metric and imperial", () => {
    render(
      <MemoryRouter>
        <Settings />
      </MemoryRouter>,
    );
    const metric = screen.getByTestId(
      "settings-region-metric",
    ) as HTMLInputElement;
    const imperial = screen.getByTestId(
      "settings-region-imperial",
    ) as HTMLInputElement;
    expect(metric.checked).toBe(true);
    fireEvent.click(imperial);
    expect(imperial.checked).toBe(true);
    expect(metric.checked).toBe(false);
  });

  it("theme toggle applies the data-theme attribute and persists to localStorage", () => {
    // Each Settings test runs in jsdom with a fresh DOM, so the
    // localStorage and <html data-theme> start clean.
    localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
    render(
      <MemoryRouter>
        <Settings />
      </MemoryRouter>,
    );
    const systemRadio = screen.getByTestId(
      "settings-theme-system",
    ) as HTMLInputElement;
    const darkRadio = screen.getByTestId(
      "settings-theme-dark",
    ) as HTMLInputElement;
    const lightRadio = screen.getByTestId(
      "settings-theme-light",
    ) as HTMLInputElement;
    // Default is "system" (no localStorage entry).
    expect(systemRadio.checked).toBe(true);
    expect(document.documentElement.hasAttribute("data-theme")).toBe(
      false,
    );
    // Switching to dark sets the attribute and persists.
    fireEvent.click(darkRadio);
    expect(darkRadio.checked).toBe(true);
    expect(document.documentElement.getAttribute("data-theme")).toBe(
      "dark",
    );
    expect(localStorage.getItem("aec.theme.mode")).toBe("dark");
    // Switching to light overrides.
    fireEvent.click(lightRadio);
    expect(lightRadio.checked).toBe(true);
    expect(document.documentElement.getAttribute("data-theme")).toBe(
      "light",
    );
    expect(localStorage.getItem("aec.theme.mode")).toBe("light");
    // Back to system removes the attribute.
    fireEvent.click(systemRadio);
    expect(systemRadio.checked).toBe(true);
    expect(document.documentElement.hasAttribute("data-theme")).toBe(
      false,
    );
    expect(localStorage.getItem("aec.theme.mode")).toBe("system");
  });
});
