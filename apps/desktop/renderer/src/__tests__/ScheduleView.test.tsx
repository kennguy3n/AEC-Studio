import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ScheduleView, ScheduleRow } from "../components/bim/ScheduleView";

const baseProps = {
  sourcePath: "/test/project.ifc",
  outPathForKind: (kind: string) => `/test/project.${kind}.xlsx`,
};

describe("ScheduleView", () => {
  it("renders all four schedule tabs and an empty body when no rows", () => {
    render(
      <ScheduleView
        {...baseProps}
        rowsByKind={{}}
        onGenerate={() => undefined}
      />,
    );
    expect(screen.getByTestId("schedule-tab-room")).toBeInTheDocument();
    expect(screen.getByTestId("schedule-tab-door")).toBeInTheDocument();
    expect(screen.getByTestId("schedule-tab-window")).toBeInTheDocument();
    expect(screen.getByTestId("schedule-tab-material")).toBeInTheDocument();
    expect(screen.getByTestId("schedule-empty")).toBeInTheDocument();
  });

  it("renders the rows table for the active tab", () => {
    const rows: ScheduleRow[] = [
      { number: "101", name: "Living", area: 23.5 },
      { number: "102", name: "Bedroom", area: 14.2 },
    ];
    render(
      <ScheduleView
        {...baseProps}
        rowsByKind={{ room: rows }}
        onGenerate={() => undefined}
      />,
    );
    const body = screen.getByTestId("schedule-rows-room");
    expect(body.querySelectorAll("tr").length).toBe(2);
  });

  it("switches to door tab when clicked", () => {
    render(
      <ScheduleView
        {...baseProps}
        rowsByKind={{}}
        onGenerate={() => undefined}
      />,
    );
    fireEvent.click(screen.getByTestId("schedule-tab-door"));
    expect(
      screen.getByTestId("schedule-tab-door").getAttribute("aria-selected"),
    ).toBe("true");
  });

  it("calls the regenerate IPC and forwards a summary to onGenerate", async () => {
    const onGenerate = vi.fn();
    render(
      <ScheduleView
        {...baseProps}
        rowsByKind={{}}
        onGenerate={onGenerate}
      />,
    );
    fireEvent.click(screen.getByTestId("schedule-regenerate"));
    await waitFor(() => expect(onGenerate).toHaveBeenCalled());
    // First arg is the kind discriminator the bridge was asked
    // to generate for; second is the summary object surfaced to
    // the parent (scheduleId / outPath / row+column counts).
    const [kind, summary] = onGenerate.mock.calls[0];
    expect(kind).toBe("room");
    expect(typeof summary.scheduleId).toBe("string");
    expect(summary.outPath).toBe("/test/project.room.xlsx");
  });
});
