import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { ICONS, Icon, type IconName } from "../icons/Icon";

/**
 * Phase 17 Group B Task 7 — Icon component coverage. The goal is
 * three-fold:
 *   1. Every icon name in `ICONS` renders without throwing and emits
 *      a `<path d=...>` whose `d` matches the source-of-truth string.
 *   2. The default `size` is 20 and the inherited stroke is
 *      `currentColor` (so `color` on a parent cascades to the icon).
 *   3. Accessibility: when no `label` is provided the SVG carries
 *      `aria-hidden="true"`; when a label is provided it gets
 *      `role="img"` + `aria-label`.
 *
 * We deliberately avoid `toMatchSnapshot()` for the icon path data —
 * snapshot output across the whole icon set would be brittle (every
 * touch-up of every icon would re-write the snapshot file). The
 * `ICONS` map IS the snapshot; what we test is "the component
 * faithfully renders each entry".
 */
describe("Icon", () => {
  it("exports a non-empty closed icon set", () => {
    const names = Object.keys(ICONS) as IconName[];
    expect(names.length).toBeGreaterThan(20);
    // Each path must be non-empty so we never render an invisible
    // glyph by accident.
    for (const n of names) {
      expect(ICONS[n].length).toBeGreaterThan(2);
    }
  });

  it.each(Object.keys(ICONS) as IconName[])(
    "renders %s with the matching path data",
    (name) => {
      render(<Icon name={name} />);
      const svg = screen.getByTestId(`icon-${name}`);
      expect(svg.tagName.toLowerCase()).toBe("svg");
      expect(svg.getAttribute("viewBox")).toBe("0 0 24 24");
      // currentColor on stroke keeps the icon under CSS cascade
      // (mode-rail .is-active / .is-active:hover / dark-mode etc.).
      expect(svg.getAttribute("stroke")).toBe("currentColor");
      const path = svg.querySelector("path");
      expect(path).not.toBeNull();
      expect(path!.getAttribute("d")).toBe(ICONS[name]);
    },
  );

  it("defaults size to 20px and strokeWidth to 2", () => {
    render(<Icon name="home" />);
    const svg = screen.getByTestId("icon-home");
    expect(svg.getAttribute("width")).toBe("20");
    expect(svg.getAttribute("height")).toBe("20");
    expect(svg.getAttribute("stroke-width")).toBe("2");
  });

  it("applies the requested size and stroke width", () => {
    render(<Icon name="home" size={32} strokeWidth={1.5} />);
    const svg = screen.getByTestId("icon-home");
    expect(svg.getAttribute("width")).toBe("32");
    expect(svg.getAttribute("height")).toBe("32");
    expect(svg.getAttribute("stroke-width")).toBe("1.5");
  });

  it("hides from a11y by default", () => {
    render(<Icon name="home" />);
    const svg = screen.getByTestId("icon-home");
    expect(svg.getAttribute("aria-hidden")).toBe("true");
    expect(svg.getAttribute("aria-label")).toBeNull();
    expect(svg.getAttribute("role")).toBeNull();
  });

  it("exposes a label for a11y when one is provided", () => {
    render(<Icon name="render" label="Render mode" />);
    const svg = screen.getByTestId("icon-render");
    expect(svg.getAttribute("aria-hidden")).toBeNull();
    expect(svg.getAttribute("aria-label")).toBe("Render mode");
    expect(svg.getAttribute("role")).toBe("img");
  });
});
