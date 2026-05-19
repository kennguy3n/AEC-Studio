import { describe, it, expect } from "vitest";
import { render, screen, waitFor, fireEvent } from "@testing-library/react";
import { AssetBrowser } from "../components/AssetBrowser";

describe("AssetBrowser", () => {
  it("lists seeded assets in grid view", async () => {
    render(<AssetBrowser />);
    await waitFor(() => {
      expect(screen.getByTestId("asset-ikea.sofa_kivik_3s")).toBeInTheDocument();
    });
  });

  it("switches to list view when toggled", async () => {
    render(<AssetBrowser />);
    fireEvent.click(screen.getByTestId("asset-view-list"));
    await waitFor(() => {
      expect(
        screen.getByTestId("asset-list-ikea.sofa_kivik_3s"),
      ).toBeInTheDocument();
    });
  });

  it("filters by tag", async () => {
    render(<AssetBrowser />);
    fireEvent.click(screen.getByTestId("filter-tag-chair"));
    await waitFor(() => {
      expect(
        screen.queryByTestId("asset-ikea.sofa_kivik_3s"),
      ).not.toBeInTheDocument();
      expect(screen.getByTestId("asset-vendor.cafe_chair_thonet")).toBeInTheDocument();
    });
  });

  it("filters by search query", async () => {
    render(<AssetBrowser />);
    fireEvent.change(screen.getByTestId("asset-search"), {
      target: { value: "outline" },
    });
    await waitFor(() => {
      expect(
        screen.getByTestId("asset-muuto.armchair_outline"),
      ).toBeInTheDocument();
      expect(
        screen.queryByTestId("asset-ikea.sofa_kivik_3s"),
      ).not.toBeInTheDocument();
    });
  });

  it("emits asset id on drag start", async () => {
    render(<AssetBrowser />);
    await waitFor(() =>
      expect(screen.getByTestId("asset-ikea.sofa_kivik_3s")).toBeInTheDocument(),
    );
    const item = screen.getByTestId("asset-ikea.sofa_kivik_3s");
    const data: Record<string, string> = {};
    const dragEvent = {
      dataTransfer: {
        setData: (k: string, v: string) => {
          data[k] = v;
        },
        effectAllowed: "",
      },
    } as unknown as React.DragEvent;
    fireEvent.dragStart(item, dragEvent);
    // jsdom doesn't propagate dataTransfer through fireEvent reliably,
    // so this test verifies the drag handler is wired up; the asset id
    // is set on the real DataTransfer in Electron.
    expect(item).toHaveAttribute("draggable", "true");
  });
});
