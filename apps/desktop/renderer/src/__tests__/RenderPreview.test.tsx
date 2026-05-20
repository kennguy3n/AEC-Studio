import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { RenderPreview } from "../components/render/RenderPreview";

describe("RenderPreview", () => {
  it("shows the empty placeholder when no image", () => {
    render(<RenderPreview imageDataUri={null} />);
    expect(screen.getByTestId("render-preview-empty")).toBeInTheDocument();
  });

  it("renders an <img> with the supplied data uri", () => {
    render(
      <RenderPreview
        imageDataUri="data:image/png;base64,AAAA"
        caption="EEVEE preview"
      />,
    );
    const img = screen.getByTestId("render-preview-image") as HTMLImageElement;
    expect(img.getAttribute("src")).toBe("data:image/png;base64,AAAA");
  });
});
