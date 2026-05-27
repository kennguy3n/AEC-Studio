import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { Draft } from "../pages/Draft";
import { ActiveProjectProvider } from "../hooks/useActiveProject";
import { ToastProvider } from "../hooks/useToast";

function renderDraft() {
  return render(
    <ToastProvider>
      <ActiveProjectProvider>
        <Draft />
      </ActiveProjectProvider>
    </ToastProvider>,
  );
}

describe("Draft page", () => {
  it("assembles toolbar, canvas, layer panel, inspector, sheet manager, command line", () => {
    renderDraft();
    expect(screen.getByTestId("draft-mode")).toBeInTheDocument();
    expect(screen.getByTestId("draft-tool-line")).toBeInTheDocument();
    expect(screen.getByTestId("draft-canvas")).toBeInTheDocument();
    expect(screen.getByTestId("layer-panel")).toBeInTheDocument();
    expect(screen.getByTestId("draft-inspector")).toBeInTheDocument();
    expect(screen.getByTestId("sheet-manager")).toBeInTheDocument();
    expect(screen.getByTestId("command-line")).toBeInTheDocument();
  });
});
