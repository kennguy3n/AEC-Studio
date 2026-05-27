import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { Bim } from "../pages/Bim";
import { ActiveProjectProvider } from "../hooks/useActiveProject";
import { ToastProvider } from "../hooks/useToast";

function renderBim() {
  return render(
    <ToastProvider>
      <ActiveProjectProvider>
        <Bim />
      </ActiveProjectProvider>
    </ToastProvider>,
  );
}

describe("Bim page", () => {
  it("assembles toolbar, spatial tree, viewport, property editor, schedule, validator", () => {
    renderBim();
    expect(screen.getByTestId("bim-mode")).toBeInTheDocument();
    expect(screen.getByTestId("bim-toolbar")).toBeInTheDocument();
    expect(screen.getByTestId("spatial-tree")).toBeInTheDocument();
    expect(screen.getByTestId("bim-viewport")).toBeInTheDocument();
    expect(screen.getByTestId("property-editor")).toBeInTheDocument();
    expect(screen.getByTestId("schedule-view")).toBeInTheDocument();
    expect(screen.getByTestId("validator-panel")).toBeInTheDocument();
  });

  it("selecting a spatial node opens the property editor for it", () => {
    renderBim();
    fireEvent.click(screen.getByTestId("spatial-node-lvl_l1"));
    expect(
      screen.getByTestId("property-editor").textContent,
    ).toContain("lvl_l1");
  });
});
