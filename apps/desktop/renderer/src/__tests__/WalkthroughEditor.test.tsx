import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import {
  WalkthroughEditor,
  WalkthroughKeyframe,
} from "../components/render/WalkthroughEditor";

const SAMPLE: WalkthroughKeyframe[] = [
  { frame: 1, position: [0, -5000, 1500], target: [0, 0, 1500] },
  { frame: 30, position: [3000, -5000, 1500], target: [0, 0, 1500] },
];

describe("WalkthroughEditor", () => {
  it("shows existing keyframes sorted by frame", () => {
    render(
      <WalkthroughEditor
        keyframes={SAMPLE}
        onChange={() => undefined}
      />,
    );
    const rows = screen.getAllByTestId(/walkthrough-keyframe-/);
    expect(rows.map((r) => r.getAttribute("data-testid"))).toEqual([
      "walkthrough-keyframe-1",
      "walkthrough-keyframe-30",
    ]);
  });

  it("adds a new keyframe and forwards via onChange", () => {
    const onChange = vi.fn();
    render(
      <WalkthroughEditor keyframes={SAMPLE} onChange={onChange} />,
    );
    // Draft frame defaults to max + 30 = 60. Click add.
    fireEvent.click(screen.getByTestId("walkthrough-add"));
    expect(onChange).toHaveBeenCalled();
    const next = onChange.mock.calls[0][0] as WalkthroughKeyframe[];
    expect(next).toHaveLength(3);
    expect(next.map((k) => k.frame)).toEqual([1, 30, 60]);
  });

  it("blocks adding a duplicate frame and shows error", () => {
    const onChange = vi.fn();
    render(
      <WalkthroughEditor keyframes={SAMPLE} onChange={onChange} />,
    );
    fireEvent.change(screen.getByTestId("walkthrough-draft-frame"), {
      target: { value: "1" },
    });
    fireEvent.click(screen.getByTestId("walkthrough-add"));
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByTestId("walkthrough-error")).toHaveTextContent(
      "Frame 1 already has a keyframe",
    );
  });

  it("removes a keyframe via the remove button", () => {
    const onChange = vi.fn();
    render(
      <WalkthroughEditor keyframes={SAMPLE} onChange={onChange} />,
    );
    fireEvent.click(screen.getByTestId("walkthrough-remove-1"));
    expect(onChange).toHaveBeenCalledWith([SAMPLE[1]]);
  });

  it("disables submit when fewer than 2 keyframes", () => {
    render(
      <WalkthroughEditor
        keyframes={[SAMPLE[0]]}
        onChange={() => undefined}
        onSubmit={() => undefined}
      />,
    );
    expect(
      (screen.getByTestId("walkthrough-submit") as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });

  it("invokes onSubmit with the keyframe list", async () => {
    const onSubmit = vi.fn().mockResolvedValue(undefined);
    render(
      <WalkthroughEditor
        keyframes={SAMPLE}
        onChange={() => undefined}
        onSubmit={onSubmit}
      />,
    );
    fireEvent.click(screen.getByTestId("walkthrough-submit"));
    await Promise.resolve();
    expect(onSubmit).toHaveBeenCalledWith(SAMPLE);
  });
});
