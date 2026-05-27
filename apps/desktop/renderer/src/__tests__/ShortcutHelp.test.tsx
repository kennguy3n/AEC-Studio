import { afterEach, describe, expect, it } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { ShortcutHelp } from "../components/ShortcutHelp";
import {
  shortcutRegistry,
  useKeyboardShortcuts,
  useShortcut,
} from "../hooks/useKeyboardShortcuts";

function Extra({
  id,
  label,
  keys,
  group,
}: {
  id: string;
  label: string;
  keys: string;
  group?: string;
}) {
  useShortcut({ id, label, keys, group, handler: () => {} });
  return null;
}

function Harness({
  extras = [],
}: {
  extras?: Array<{ id: string; label: string; keys: string; group?: string }>;
}) {
  useKeyboardShortcuts();
  return (
    <>
      {extras.map((s) => (
        <Extra key={s.id} {...s} />
      ))}
      <ShortcutHelp />
    </>
  );
}

afterEach(() => {
  shortcutRegistry.reset();
});

describe("<ShortcutHelp>", () => {
  it("renders nothing by default", () => {
    render(<Harness />);
    expect(screen.queryByTestId("shortcut-help")).not.toBeInTheDocument();
  });

  it("opens via the '?' shortcut and lists registered commands", () => {
    render(
      <Harness
        extras={[
          {
            id: "go-design",
            label: "Go to Design",
            group: "navigation",
            keys: "mod+2",
          },
          { id: "undo", label: "Undo", group: "global", keys: "mod+z" },
        ]}
      />,
    );
    fireEvent.keyDown(window, { key: "?", shiftKey: true });
    expect(screen.getByTestId("shortcut-help")).toBeInTheDocument();
    expect(screen.getByTestId("shortcut-group-navigation")).toHaveTextContent(
      "Go to Design",
    );
    expect(screen.getByTestId("shortcut-group-global")).toHaveTextContent(
      "Undo",
    );
  });

  it("closes via the close button", () => {
    render(<Harness />);
    fireEvent.keyDown(window, { key: "?", shiftKey: true });
    expect(screen.getByTestId("shortcut-help")).toBeInTheDocument();
    fireEvent.click(screen.getByTestId("shortcut-help-close"));
    expect(screen.queryByTestId("shortcut-help")).not.toBeInTheDocument();
  });

  it("renders 'Ctrl' on non-mac platforms for the `mod` accelerator", () => {
    // JSDOM's default navigator.platform is "Linux x86_64".
    render(
      <Harness
        extras={[{ id: "undo", label: "Undo", group: "global", keys: "mod+z" }]}
      />,
    );
    fireEvent.keyDown(window, { key: "?", shiftKey: true });
    const text = screen.getByTestId("shortcut-group-global").textContent ?? "";
    expect(text).toContain("Ctrl");
    expect(text).not.toContain("⌘");
  });

  it("renders '⌘' on macOS for the `mod` accelerator", () => {
    const originalPlatform = navigator.platform;
    Object.defineProperty(navigator, "platform", {
      value: "MacIntel",
      configurable: true,
    });
    try {
      render(
        <Harness
          extras={[
            { id: "undo", label: "Undo", group: "global", keys: "mod+z" },
          ]}
        />,
      );
      fireEvent.keyDown(window, { key: "?", shiftKey: true });
      const text =
        screen.getByTestId("shortcut-group-global").textContent ?? "";
      expect(text).toContain("⌘");
      expect(text).not.toContain("Ctrl");
    } finally {
      Object.defineProperty(navigator, "platform", {
        value: originalPlatform,
        configurable: true,
      });
    }
  });
});
