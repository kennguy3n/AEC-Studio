import { describe, it, expect } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { BeforeAfterCompare } from "../components/render/BeforeAfterCompare";

describe("BeforeAfterCompare", () => {
  it("shows the empty state when one image is missing", () => {
    render(<BeforeAfterCompare before={null} after={null} />);
    expect(screen.getByTestId("before-after-compare").textContent).toContain(
      "Pick two renders",
    );
  });

  it("renders before/after images and a slider when both supplied", () => {
    render(
      <BeforeAfterCompare
        before="data:image/png;base64,BBB"
        after="data:image/png;base64,AAA"
      />,
    );
    expect(screen.getByTestId("render-compare-before")).toBeInTheDocument();
    expect(screen.getByTestId("render-compare-after")).toBeInTheDocument();
    const slider = screen.getByTestId(
      "render-compare-slider",
    ) as HTMLInputElement;
    expect(slider.value).toBe("50");
    fireEvent.change(slider, { target: { value: "20" } });
    expect(slider.value).toBe("20");
  });

  it("displays the SSIM percentage when a finite score is provided", () => {
    render(
      <BeforeAfterCompare
        before="data:image/png;base64,BBB"
        after="data:image/png;base64,AAA"
        ssim={0.9876}
      />,
    );
    expect(screen.getByTestId("render-compare-ssim").textContent).toContain(
      "SSIM: 98.8%",
    );
  });

  it("hides the SSIM badge for null / non-finite scores", () => {
    render(
      <BeforeAfterCompare
        before="data:image/png;base64,BBB"
        after="data:image/png;base64,AAA"
        ssim={null}
      />,
    );
    expect(screen.queryByTestId("render-compare-ssim")).toBeNull();
  });
});
