import { describe, it, expect, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { Settings } from "../pages/Settings";

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

  it("KChat toggle is off by default and persists toggle", () => {
    render(
      <MemoryRouter>
        <Settings />
      </MemoryRouter>,
    );
    const toggle = screen.getByTestId(
      "settings-kchat-enabled",
    ) as HTMLInputElement;
    expect(toggle.checked).toBe(false);
    fireEvent.click(toggle);
    expect(toggle.checked).toBe(true);
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
});
