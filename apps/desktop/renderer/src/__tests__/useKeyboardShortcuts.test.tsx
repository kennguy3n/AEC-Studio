import { afterEach, describe, expect, it, vi } from "vitest";
import { renderHook, act } from "@testing-library/react";
import { fireEvent } from "@testing-library/react";
import {
  eventToKeys,
  shortcutRegistry,
  useKeyboardShortcuts,
  useShortcut,
} from "../hooks/useKeyboardShortcuts";

afterEach(() => {
  shortcutRegistry.reset();
});

describe("eventToKeys", () => {
  it("collapses ctrl and meta to mod", () => {
    const ctrlS = new KeyboardEvent("keydown", { key: "s", ctrlKey: true });
    const cmdS = new KeyboardEvent("keydown", { key: "s", metaKey: true });
    expect(eventToKeys(ctrlS)).toBe("mod+s");
    expect(eventToKeys(cmdS)).toBe("mod+s");
  });

  it("emits empty string for lone modifier presses", () => {
    expect(
      eventToKeys(new KeyboardEvent("keydown", { key: "Control" })),
    ).toBe("");
    expect(eventToKeys(new KeyboardEvent("keydown", { key: "Shift" }))).toBe(
      "",
    );
  });

  it("preserves shift and alt and lowercases the key", () => {
    expect(
      eventToKeys(
        new KeyboardEvent("keydown", {
          key: "P",
          ctrlKey: true,
          shiftKey: true,
        }),
      ),
    ).toBe("mod+shift+p");
  });
});

describe("shortcutRegistry", () => {
  it("registers and unregisters a command", () => {
    const handler = vi.fn();
    const dispose = shortcutRegistry.register({
      id: "test-cmd",
      label: "Test command",
      keys: "mod+t",
      handler,
    });
    expect(shortcutRegistry.findByKeys("mod+t")?.id).toBe("test-cmd");
    dispose();
    expect(shortcutRegistry.findByKeys("mod+t")).toBeUndefined();
  });

  it("replaces an existing command on re-register", () => {
    const a = vi.fn();
    const b = vi.fn();
    shortcutRegistry.register({
      id: "dup",
      label: "A",
      keys: "mod+d",
      handler: a,
    });
    shortcutRegistry.register({
      id: "dup",
      label: "B",
      keys: "mod+d",
      handler: b,
    });
    const matches = shortcutRegistry.all().filter((c) => c.id === "dup");
    expect(matches.length).toBe(1);
    expect(matches[0].label).toBe("B");
  });
});

describe("useKeyboardShortcuts", () => {
  it("fires the matching handler on global keydown", () => {
    const handler = vi.fn();
    renderHook(() => {
      useKeyboardShortcuts();
      useShortcut({
        id: "render",
        label: "Render",
        keys: "mod+r",
        handler,
      });
    });
    act(() => {
      fireEvent.keyDown(window, { key: "r", ctrlKey: true });
    });
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it("skips form-focused targets unless whenInputFocused is set", () => {
    const blocked = vi.fn();
    const allowed = vi.fn();
    renderHook(() => {
      useKeyboardShortcuts();
      useShortcut({
        id: "search",
        label: "Search",
        keys: "mod+f",
        handler: blocked,
      });
      useShortcut({
        id: "palette",
        label: "Palette",
        keys: "mod+k",
        handler: allowed,
        whenInputFocused: true,
      });
    });

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    act(() => {
      fireEvent.keyDown(input, { key: "f", ctrlKey: true });
    });
    expect(blocked).not.toHaveBeenCalled();

    act(() => {
      fireEvent.keyDown(input, { key: "k", ctrlKey: true });
    });
    expect(allowed).toHaveBeenCalledTimes(1);
    document.body.removeChild(input);
  });

  it("picks up live changes to whenInputFocused without re-keying", () => {
    // Regression: previously `useShortcut` captured `whenInputFocused`
    // at first registration and only re-registered when `id` or `keys`
    // changed. Toggling the flag between renders should now take
    // effect immediately because the registered entry reads through a
    // ref.
    const fired = vi.fn();
    const { rerender } = renderHook(
      ({ allowInForms }: { allowInForms: boolean }) => {
        useKeyboardShortcuts();
        useShortcut({
          id: "live-focus",
          label: "Live focus",
          keys: "mod+j",
          handler: fired,
          whenInputFocused: allowInForms,
        });
      },
      { initialProps: { allowInForms: false } },
    );

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();

    act(() => {
      fireEvent.keyDown(input, { key: "j", ctrlKey: true });
    });
    expect(fired).not.toHaveBeenCalled();

    rerender({ allowInForms: true });

    act(() => {
      fireEvent.keyDown(input, { key: "j", ctrlKey: true });
    });
    expect(fired).toHaveBeenCalledTimes(1);

    document.body.removeChild(input);
  });

  it("exposes the latest label and handler to registry consumers", () => {
    // The CommandPalette renders `shortcutRegistry.all()`. If a label
    // changes across renders, the palette must see the new value.
    const { rerender } = renderHook(
      ({ label, handler }: { label: string; handler: () => void }) => {
        useShortcut({
          id: "live-label",
          label,
          keys: "mod+i",
          handler,
        });
      },
      {
        initialProps: { label: "First label", handler: vi.fn() },
      },
    );

    expect(shortcutRegistry.findByKeys("mod+i")?.label).toBe("First label");

    const newHandler = vi.fn();
    rerender({ label: "Second label", handler: newHandler });

    const entry = shortcutRegistry.findByKeys("mod+i");
    expect(entry?.label).toBe("Second label");
    entry?.handler();
    expect(newHandler).toHaveBeenCalledTimes(1);
  });
});
