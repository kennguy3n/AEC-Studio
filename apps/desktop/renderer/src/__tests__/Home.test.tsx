import { describe, it, expect } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { Home } from "../pages/Home";

describe("Home page", () => {
  it("shows the dashboard heading and hardware card", async () => {
    render(
      <MemoryRouter>
        <Home />
      </MemoryRouter>,
    );
    expect(screen.getByText("AEC Studio")).toBeInTheDocument();
    await waitFor(() => {
      expect(screen.getByLabelText("Hardware profile")).toBeInTheDocument();
    });
  });

  it("renders all 8 template cards", () => {
    render(
      <MemoryRouter>
        <Home />
      </MemoryRouter>,
    );
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
    render(
      <MemoryRouter>
        <Home />
      </MemoryRouter>,
    );
    const apartment = screen.getByTestId("template-interior.apartment");
    const newBtn = apartment.querySelector("button");
    expect(newBtn).not.toBeNull();
    fireEvent.click(newBtn!);
    await waitFor(() => {
      expect(
        screen.getByText(/Apartment Project/),
      ).toBeInTheDocument();
    });
  });
});
