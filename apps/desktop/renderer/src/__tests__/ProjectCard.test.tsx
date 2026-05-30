import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, cleanup } from "@testing-library/react";

import { ProjectCard } from "../components/ProjectCard";
import { aec } from "../api/aec";
import type { ProjectSummary, ProjectThumbnail } from "../api/aec";

/**
 * Phase 17 Group B Task 12 — `ProjectCard` thumbnail wiring.
 *
 * Coverage:
 *   1. Card renders a placeholder slot (empty `.project-card__thumb`)
 *      while the thumbnail fetch is in flight.
 *   2. On hit, an `<img>` element is rendered with a `blob:` URL
 *      derived from the bridge's PNG bytes. `width` / `height`
 *      attributes mirror the persisted thumbnail dimensions so the
 *      layout doesn't shift on decode.
 *   3. On miss (bridge returns `null`), the gradient placeholder
 *      stays visible and no `<img>` is mounted.
 *   4. When the project's `modifiedAt` advances, the card re-fetches
 *      and replaces the previous `blob:` URL (revoking the old one
 *      so we don't leak per-save).
 *   5. Unmount revokes the active `blob:` URL.
 *
 * `URL.createObjectURL` / `URL.revokeObjectURL` and `Blob` are all
 * provided by jsdom 24, so we exercise the real DOM API rather
 * than a mock — only the bridge fetch (`aec.project.getThumbnail`)
 * is stubbed.
 */

const FIXED_PNG = new Uint8Array([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // signature
  0x00, 0x00, 0x00, 0x0d, // IHDR length
  0x49, 0x48, 0x44, 0x52, // "IHDR"
  0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1×1
  0x08, 0x06, 0x00, 0x00, 0x00, // depth / color / etc.
  0x1f, 0x15, 0xc4, 0x89, // CRC
]);

function summary(overrides: Partial<ProjectSummary> = {}): ProjectSummary {
  return {
    projectId: "proj-1",
    name: "Apartment in Helsinki",
    path: "/projects/apt-hel.aecstudio",
    templateKey: "interior.apartment",
    modifiedAt: "2025-05-30T08:00:00.000Z",
    ...overrides,
  };
}

describe("ProjectCard thumbnail", () => {
  let createdUrls: string[];
  let revokedUrls: string[];
  let originalCreateObjectURL: typeof URL.createObjectURL | undefined;
  let originalRevokeObjectURL: typeof URL.revokeObjectURL | undefined;
  // `vi.spyOn(obj, "key")` returns a `MockInstance` whose arg/return
  // types track the spied method's exact signature. The generic
  // `ReturnType<typeof vi.spyOn>` resolves to
  // `MockInstance<unknown[], unknown>`, which is contravariantly
  // incompatible with the narrower `MockInstance<[string], …>` we
  // get back from a real spy on `getThumbnail` (TS rejects the
  // arg-type widening). We only ever need `mockRestore()` on this
  // holder, so type it via the spy's `mockRestore` shape — that's
  // the only method the cleanup hook touches.
  let getThumbnailSpy: { mockRestore: () => void } | undefined;

  beforeEach(() => {
    createdUrls = [];
    revokedUrls = [];
    // jsdom 24 omits `URL.createObjectURL` / `URL.revokeObjectURL`
    // entirely (the spec considers them browser-only). We install a
    // stable, observable pair to verify create/revoke pairing; the
    // restore step in `afterEach` puts the original (possibly
    // `undefined`) descriptors back so other suites in the same
    // worker see jsdom's default behaviour.
    originalCreateObjectURL = URL.createObjectURL as
      | typeof URL.createObjectURL
      | undefined;
    originalRevokeObjectURL = URL.revokeObjectURL as
      | typeof URL.revokeObjectURL
      | undefined;
    URL.createObjectURL = vi.fn((blob: Blob | MediaSource) => {
      const url = `blob:test:${createdUrls.length}`;
      createdUrls.push(url);
      // Defensive: confirm the caller really did hand us a Blob with
      // PNG-type so a regression that passes raw bytes would surface
      // as a test failure rather than a runtime warning.
      if (blob instanceof Blob) {
        expect(blob.type).toBe("image/png");
      }
      return url;
    }) as typeof URL.createObjectURL;
    URL.revokeObjectURL = vi.fn((url: string) => {
      revokedUrls.push(url);
    });
  });

  afterEach(() => {
    // Unmount any rendered cards BEFORE restoring URL.* — React's
    // unmount path runs `ProjectCard`'s effect cleanup which calls
    // `URL.revokeObjectURL`. If we restored first, the cleanup
    // would call the (possibly missing) jsdom default and throw.
    cleanup();
    if (originalCreateObjectURL !== undefined) {
      URL.createObjectURL = originalCreateObjectURL;
    } else {
      // jsdom didn't provide one — delete so the next test starts
      // from a clean slate.
      delete (URL as unknown as { createObjectURL?: unknown }).createObjectURL;
    }
    if (originalRevokeObjectURL !== undefined) {
      URL.revokeObjectURL = originalRevokeObjectURL;
    } else {
      delete (URL as unknown as { revokeObjectURL?: unknown }).revokeObjectURL;
    }
    getThumbnailSpy?.mockRestore();
  });

  it("renders an <img> with bytes from the bridge", async () => {
    const t: ProjectThumbnail = {
      png: FIXED_PNG,
      width: 256,
      height: 192,
      updatedAt: "2025-05-30T08:00:01.000Z",
    };
    getThumbnailSpy = vi
      .spyOn(aec.project, "getThumbnail")
      .mockResolvedValue(t);

    render(<ProjectCard project={summary()} />);

    const img = (await screen.findByTestId(
      "project-card-thumb-img",
    )) as HTMLImageElement;
    expect(img.src).toBe(createdUrls[0]);
    expect(img.getAttribute("width")).toBe("256");
    expect(img.getAttribute("height")).toBe("192");
    expect(img.getAttribute("alt")).toBe("");
    expect(img.getAttribute("decoding")).toBe("async");
    expect(img.getAttribute("loading")).toBe("lazy");
    expect(getThumbnailSpy).toHaveBeenCalledWith(summary().path);
  });

  it("falls back to the gradient placeholder on bridge miss", async () => {
    getThumbnailSpy = vi
      .spyOn(aec.project, "getThumbnail")
      .mockResolvedValue(null);

    render(<ProjectCard project={summary()} />);

    // Wait one tick so the effect's then-branch resolves.
    await waitFor(() => {
      expect(getThumbnailSpy).toHaveBeenCalled();
    });
    expect(screen.queryByTestId("project-card-thumb-img")).toBeNull();
    expect(URL.createObjectURL).not.toHaveBeenCalled();
  });

  it("swallows bridge errors so the grid never breaks", async () => {
    getThumbnailSpy = vi
      .spyOn(aec.project, "getThumbnail")
      .mockRejectedValue(new Error("simulated bridge failure"));

    render(<ProjectCard project={summary()} />);
    await waitFor(() => {
      expect(getThumbnailSpy).toHaveBeenCalled();
    });
    expect(screen.queryByTestId("project-card-thumb-img")).toBeNull();
  });

  it("re-fetches and swaps the URL when modifiedAt advances", async () => {
    const first: ProjectThumbnail = {
      png: FIXED_PNG,
      width: 256,
      height: 192,
      updatedAt: "2025-05-30T08:00:01.000Z",
    };
    const second: ProjectThumbnail = {
      png: FIXED_PNG,
      width: 512,
      height: 384,
      updatedAt: "2025-05-30T08:00:02.000Z",
    };
    getThumbnailSpy = vi
      .spyOn(aec.project, "getThumbnail")
      .mockResolvedValueOnce(first)
      .mockResolvedValueOnce(second);

    const initial = summary({ modifiedAt: "2025-05-30T08:00:00.000Z" });
    const { rerender } = render(<ProjectCard project={initial} />);

    await screen.findByTestId("project-card-thumb-img");
    expect(createdUrls).toHaveLength(1);
    const firstUrl = createdUrls[0];

    rerender(
      <ProjectCard
        project={summary({ modifiedAt: "2025-05-30T08:00:05.000Z" })}
      />,
    );

    // Wait until the second fetch's blob URL replaces the first.
    await waitFor(() => {
      const img = screen.getByTestId(
        "project-card-thumb-img",
      ) as HTMLImageElement;
      expect(img.src).toBe(createdUrls[1]);
    });
    // The first URL must have been revoked when the second one
    // replaced it — otherwise we'd leak a blob URL per save tick.
    expect(revokedUrls).toContain(firstUrl);
  });

  it("revokes the active blob URL on unmount", async () => {
    const t: ProjectThumbnail = {
      png: FIXED_PNG,
      width: 256,
      height: 192,
      updatedAt: "2025-05-30T08:00:01.000Z",
    };
    getThumbnailSpy = vi
      .spyOn(aec.project, "getThumbnail")
      .mockResolvedValue(t);

    const { unmount } = render(<ProjectCard project={summary()} />);
    await screen.findByTestId("project-card-thumb-img");
    const created = createdUrls[0];
    unmount();
    expect(revokedUrls).toContain(created);
  });
});
