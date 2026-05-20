import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import {
  CommandPalette,
  filterCommands,
} from "../components/CommandPalette";
import { shortcutRegistry } from "../hooks/useKeyboardShortcuts";

afterEach(() => {
  shortcutRegistry.reset();
});

const sampleCommands = [
  {
    id: "render",
    label: "Queue render",
    keys: "mod+r",
    group: "render",
    handler: vi.fn(),
  },
  {
    id: "save",
    label: "Save project",
    keys: "mod+s",
    group: "global",
    handler: vi.fn(),
  },
  {
    id: "deliver",
    label: "Build contractor pack",
    keys: "mod+e",
    group: "deliver",
    handler: vi.fn(),
  },
];

describe("filterCommands", () => {
  it("returns all commands when query is empty", () => {
    expect(filterCommands(sampleCommands, "")).toHaveLength(3);
  });

  it("ranks exact substring matches first", () => {
    const matches = filterCommands(sampleCommands, "render");
    expect(matches[0].id).toBe("render");
  });

  it("supports subsequence matches", () => {
    // 'qr' is a subsequence of 'Queue render' but not of 'Save project'.
    const matches = filterCommands(sampleCommands, "qr");
    expect(matches.some((m) => m.id === "render")).toBe(true);
    expect(matches.some((m) => m.id === "save")).toBe(false);
  });

  it("filters out non-matching entries", () => {
    expect(filterCommands(sampleCommands, "xyz")).toEqual([]);
  });
});

describe("CommandPalette", () => {
  it("renders nothing when closed", () => {
    render(
      <CommandPalette
        open={false}
        onClose={() => {}}
        commands={sampleCommands}
      />,
    );
    expect(
      screen.queryByTestId("command-palette-overlay"),
    ).not.toBeInTheDocument();
  });

  it("renders all commands when open with empty query", () => {
    render(
      <CommandPalette
        open
        onClose={() => {}}
        commands={sampleCommands}
      />,
    );
    expect(screen.getByTestId("command-palette")).toBeInTheDocument();
    expect(
      screen.getByTestId("command-palette-item-render"),
    ).toBeInTheDocument();
    expect(
      screen.getByTestId("command-palette-item-save"),
    ).toBeInTheDocument();
    expect(
      screen.getByTestId("command-palette-item-deliver"),
    ).toBeInTheDocument();
  });

  it("filters results as the user types", () => {
    render(
      <CommandPalette
        open
        onClose={() => {}}
        commands={sampleCommands}
      />,
    );
    const input = screen.getByTestId(
      "command-palette-input",
    ) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "render" } });
    expect(
      screen.getByTestId("command-palette-item-render"),
    ).toBeInTheDocument();
    expect(
      screen.queryByTestId("command-palette-item-save"),
    ).not.toBeInTheDocument();
  });

  it("fires the active command on Enter and closes", () => {
    const handler = vi.fn();
    const onClose = vi.fn();
    const cmds = [
      {
        id: "go",
        label: "Go",
        keys: "mod+g",
        handler,
      },
    ];
    render(<CommandPalette open onClose={onClose} commands={cmds} />);
    const input = screen.getByTestId(
      "command-palette-input",
    ) as HTMLInputElement;
    fireEvent.keyDown(input, { key: "Enter" });
    expect(handler).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("closes on Escape", () => {
    const onClose = vi.fn();
    render(
      <CommandPalette
        open
        onClose={onClose}
        commands={sampleCommands}
      />,
    );
    const input = screen.getByTestId(
      "command-palette-input",
    ) as HTMLInputElement;
    fireEvent.keyDown(input, { key: "Escape" });
    expect(onClose).toHaveBeenCalled();
  });

  it("shows empty-state when nothing matches", () => {
    render(
      <CommandPalette
        open
        onClose={() => {}}
        commands={sampleCommands}
      />,
    );
    const input = screen.getByTestId(
      "command-palette-input",
    ) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "zzz" } });
    expect(screen.getByTestId("command-palette-empty")).toBeInTheDocument();
  });
});
