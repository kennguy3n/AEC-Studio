import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { BimToolbar } from "../components/bim/BimToolbar";

describe("BimToolbar", () => {
  it("renders all action buttons", () => {
    render(<BimToolbar busyAction={null} onInvoke={() => undefined} />);
    for (const id of [
      "importIfc",
      "exportIfc",
      "validate",
      "classify",
      "generateSchedule",
      "diff",
      "boq",
    ]) {
      expect(screen.getByTestId(`bim-action-${id}`)).toBeInTheDocument();
    }
  });

  it("disables non-busy actions when one is in flight", () => {
    render(
      <BimToolbar busyAction="importIfc" onInvoke={() => undefined} />,
    );
    expect(
      (screen.getByTestId("bim-action-importIfc") as HTMLButtonElement)
        .disabled,
    ).toBe(false);
    expect(
      (screen.getByTestId("bim-action-validate") as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });

  it("invokes onInvoke when an action button is clicked", () => {
    const onInvoke = vi.fn();
    render(<BimToolbar busyAction={null} onInvoke={onInvoke} />);
    fireEvent.click(screen.getByTestId("bim-action-validate"));
    expect(onInvoke).toHaveBeenCalledWith("validate");
  });
});
