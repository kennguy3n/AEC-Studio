import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { act, render, screen } from "@testing-library/react";
import {
  TemplateCard,
  type TemplateChoice,
  DEFAULT_TEMPLATES,
} from "../components/TemplateCard";

/**
 * Phase 17 Group B Tasks 7 + 13 — TemplateCard preview behavior.
 *
 * The card resolves a preview image lazily via `fetch(previewPath)`
 * in a `useEffect`. We exercise both branches:
 *   1. Fetch returns ok → the `<img>` is rendered, not the icon.
 *   2. Fetch returns non-ok / rejects → the SVG icon is rendered
 *      and `data-has-preview` reads `"false"`.
 * Both branches must end up with `previewSrc` set to the same value
 * the first render committed; that prevents an SSR / first-paint
 * flicker where the icon shows for one frame, then is replaced by
 * the image.
 */
describe("TemplateCard", () => {
  const sample: TemplateChoice = {
    key: "test.sample",
    name: "Sample",
    description: "A test template",
    category: "interior",
    icon: "home",
    previewPath: "/templates/test.sample/preview.png",
  };

  beforeEach(() => {
    // Replace the global fetch on each test so we can independently
    // control the ok/non-ok branches. `vi.stubGlobal` cleanly
    // restores via `vi.unstubAllGlobals` in afterEach.
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("renders the icon fallback when preview fetch fails", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({ ok: false, status: 404 }),
    );
    await act(async () => {
      render(<TemplateCard template={sample} />);
    });
    const hero = screen.getByTestId(`template-hero-${sample.key}`);
    expect(hero.getAttribute("data-has-preview")).toBe("false");
    // The fallback Icon must render; the preview <img> must not.
    expect(
      screen.queryByTestId(`template-preview-${sample.key}`),
    ).toBeNull();
    expect(screen.getByTestId("icon-home")).toBeInTheDocument();
  });

  it("renders an <img> when preview fetch succeeds", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue({ ok: true, status: 200 }),
    );
    await act(async () => {
      render(<TemplateCard template={sample} />);
    });
    const hero = screen.getByTestId(`template-hero-${sample.key}`);
    // useEffect runs synchronously under act() — the data-attr should
    // be "true" by the time we inspect it.
    expect(hero.getAttribute("data-has-preview")).toBe("true");
    const img = screen.getByTestId(
      `template-preview-${sample.key}`,
    ) as HTMLImageElement;
    expect(img.src).toContain(sample.previewPath!);
  });

  it("ships an icon for every default template", () => {
    expect(DEFAULT_TEMPLATES.length).toBeGreaterThan(0);
    for (const t of DEFAULT_TEMPLATES) {
      expect(t.icon).toBeTruthy();
      expect(t.name).toBeTruthy();
    }
  });

  it("invokes onCreate when the New Project button is clicked", () => {
    const spy = vi.fn();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: false }));
    render(<TemplateCard template={sample} onCreate={spy} />);
    const btn = screen.getByRole("button", { name: /new project/i });
    btn.click();
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy).toHaveBeenCalledWith(sample);
  });
});
