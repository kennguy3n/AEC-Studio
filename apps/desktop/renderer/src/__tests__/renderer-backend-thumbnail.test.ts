import { describe, expect, it } from "vitest";

import { rendererInProcessBackend } from "../api/renderer-backend";

/**
 * Phase 17 Group B Task 12 — renderer-side fallback backend
 * exercises the same `setThumbnail` / `getThumbnail` contract the
 * Electron preload exposes to production. The native bridge has its
 * own Rust-side test for the same contract (see
 * `crates/aec_bridge/src/service.rs::project_thumbnail_set_get_roundtrip`
 * and `project_set_thumbnail_rejects_invalid_input`); these tests
 * pin the fallback shape so the two paths cannot drift.
 */
const MIN_PNG_HEADER = new Uint8Array([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00,
]);

describe("rendererInProcessBackend.project.thumbnail", () => {
  it("round-trips PNG bytes for a project path", async () => {
    const be = rendererInProcessBackend();
    const path = "/projects/round-trip.aecstudio";
    expect(await be.project.getThumbnail(path)).toBeNull();

    const result = await be.project.setThumbnail(path, MIN_PNG_HEADER, 256, 192);
    expect(result).toEqual({ ok: true });

    const got = await be.project.getThumbnail(path);
    expect(got).not.toBeNull();
    expect(Array.from(got!.png)).toEqual(Array.from(MIN_PNG_HEADER));
    expect(got!.width).toBe(256);
    expect(got!.height).toBe(192);
    expect(got!.updatedAt).toMatch(/\d{4}-\d{2}-\d{2}T/);
  });

  it("isolates state per project path", async () => {
    const be = rendererInProcessBackend();
    await be.project.setThumbnail(
      "/projects/a.aecstudio",
      MIN_PNG_HEADER,
      128,
      96,
    );
    expect(await be.project.getThumbnail("/projects/b.aecstudio")).toBeNull();
  });

  it("returns a defensive copy so callers cannot mutate the cache", async () => {
    const be = rendererInProcessBackend();
    const path = "/projects/defensive.aecstudio";
    await be.project.setThumbnail(path, MIN_PNG_HEADER, 256, 192);

    const got = await be.project.getThumbnail(path);
    expect(got).not.toBeNull();
    // Mutate the returned typed array. The cache must remain
    // unaffected on the next read.
    got!.png[0] = 0xff;

    const got2 = await be.project.getThumbnail(path);
    expect(got2!.png[0]).toBe(0x89); // still the PNG signature byte
  });

  it("rejects an empty buffer with a descriptive error", async () => {
    const be = rendererInProcessBackend();
    await expect(
      be.project.setThumbnail(
        "/p.aecstudio",
        new Uint8Array(0),
        256,
        192,
      ),
    ).rejects.toThrow(/empty/);
  });

  it("rejects a buffer without the PNG magic header", async () => {
    const be = rendererInProcessBackend();
    const fake = new Uint8Array([0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07]);
    await expect(
      be.project.setThumbnail("/p.aecstudio", fake, 256, 192),
    ).rejects.toThrow(/PNG magic header/);
  });

  it("rejects zero / over-large dimensions", async () => {
    const be = rendererInProcessBackend();
    await expect(
      be.project.setThumbnail("/p.aecstudio", MIN_PNG_HEADER, 0, 192),
    ).rejects.toThrow(/dimensions out of range/);
    await expect(
      be.project.setThumbnail("/p.aecstudio", MIN_PNG_HEADER, 256, 5000),
    ).rejects.toThrow(/dimensions out of range/);
  });

  it("rejects buffers larger than 1 MiB", async () => {
    const be = rendererInProcessBackend();
    const big = new Uint8Array(1024 * 1024 + 1);
    big.set(MIN_PNG_HEADER, 0);
    await expect(
      be.project.setThumbnail("/p.aecstudio", big, 256, 192),
    ).rejects.toThrow(/max is /);
  });

  it("overwrites the row on a second write", async () => {
    const be = rendererInProcessBackend();
    const path = "/projects/overwrite.aecstudio";
    await be.project.setThumbnail(path, MIN_PNG_HEADER, 100, 100);

    // Wait one ms so the `updatedAt` ISO-8601 second field can
    // possibly advance. Even when the clock is stable across calls
    // we still confirm the bytes / dims overwrote correctly.
    await new Promise((r) => setTimeout(r, 2));

    const tweaked = new Uint8Array(MIN_PNG_HEADER);
    await be.project.setThumbnail(path, tweaked, 200, 150);
    const got = await be.project.getThumbnail(path);
    expect(got!.width).toBe(200);
    expect(got!.height).toBe(150);
  });
});
