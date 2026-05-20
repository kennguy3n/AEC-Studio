import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { SheetManager, SheetTab } from "../components/draft/SheetManager";

describe("SheetManager", () => {
  it("renders tabs for each sheet", () => {
    const sheets: SheetTab[] = [
      { id: "s1", name: "Floor 1" },
      { id: "s2", name: "Floor 2" },
    ];
    render(<SheetManager sheets={sheets} activeId="s1" onChange={() => {}} />);
    expect(screen.getByTestId("sheet-tab-s1")).toBeInTheDocument();
    expect(screen.getByTestId("sheet-tab-s2")).toBeInTheDocument();
    expect(screen.getByTestId("sheet-tab-s1")).toHaveAttribute("aria-selected", "true");
  });

  it("creates a new sheet via the create button", async () => {
    const spy = vi.fn<[sheets: SheetTab[], activeId: string | null], void>();
    render(<SheetManager sheets={[]} activeId={null} onChange={spy} />);
    fireEvent.click(screen.getByTestId("sheet-create"));
    await waitFor(() => expect(spy).toHaveBeenCalled());
    const [next, active] = spy.mock.calls.at(-1) ?? [];
    expect(next?.length).toBe(1);
    expect(active).toBe(next?.[0]?.id);
  });

  it("removes a sheet via delete button", () => {
    const sheets: SheetTab[] = [
      { id: "s1", name: "A" },
      { id: "s2", name: "B" },
    ];
    const spy = vi.fn<[sheets: SheetTab[], activeId: string | null], void>();
    render(<SheetManager sheets={sheets} activeId="s1" onChange={spy} />);
    fireEvent.click(screen.getByLabelText("Delete A"));
    expect(spy).toHaveBeenCalled();
    const [next, active] = spy.mock.calls.at(-1) ?? [];
    expect(next?.length).toBe(1);
    expect(next?.[0]?.id).toBe("s2");
    expect(active).toBe("s2");
  });
});
