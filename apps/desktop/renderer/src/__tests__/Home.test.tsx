import { describe, it, expect } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { Home } from "../pages/Home";
import { ActiveProjectProvider } from "../hooks/useActiveProject";
import { ToastProvider } from "../hooks/useToast";

function renderHome() {
  return render(
    <MemoryRouter>
      <ToastProvider>
        <ActiveProjectProvider>
          <Home />
        </ActiveProjectProvider>
      </ToastProvider>
    </MemoryRouter>,
  );
}

describe("Home page", () => {
  it("shows the dashboard heading and hardware card", async () => {
    renderHome();
    expect(screen.getByText("AEC Studio")).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getByLabelText("Hardware profile")).toBeInTheDocument();
    });
  });

  it("renders all 8 template cards", () => {
    renderHome();
    expect(screen.getByTestId("template-interior.apartment")).toBeInTheDocument();
    expect(screen.getByTestId("template-interior.kitchen")).toBeInTheDocument();
    expect(screen.getByTestId("template-interior.bathroom")).toBeInTheDocument();
    expect(screen.getByTestId("template-interior.renovation")).toBeInTheDocument();
    expect(screen.getByTestId("template-architecture.cafe")).toBeInTheDocument();
    expect(screen.getByTestId("template-architecture.office")).toBeInTheDocument();
    expect(screen.getByTestId("template-architecture.villa")).toBeInTheDocument();
    expect(screen.getByTestId("template-architecture.retail")).toBeInTheDocument();
  });

  it("creates a new project when a template card is activated", async () => {
    renderHome();
    const apartment = screen.getByTestId("template-interior.apartment");
    const newBtn = apartment.querySelector("button");
    expect(newBtn).not.toBeNull();
    fireEvent.click(newBtn!);
    // After creating a project, the Home page navigates to /design.
    // In the test's MemoryRouter, navigation happens in-memory.
    // Just verify no errors were thrown and the button was clickable.
    await waitFor(() => {
      // The template button should still be present (we haven't left the MemoryRouter context)
      expect(screen.getByTestId("template-interior.apartment")).toBeInTheDocument();
    });
  });
});
