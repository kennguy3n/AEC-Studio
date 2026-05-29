/**
 * Unit tests for the .kcz build pipeline helpers.
 *
 * The end-to-end build (which runs `tsc` then writes the archive
 * into `releases/`) is exercised by `npm run build` in CI; these
 * tests focus on the pure helpers — zip writer + manifest entry-
 * point validation — so a regression in either is caught without
 * having to invoke the TypeScript compiler.
 */
import assert from "node:assert/strict";
import { deflateRawSync, inflateRawSync } from "node:zlib";
import { test } from "node:test";
import { buildKczZip } from "./zipWriter.mjs";

test("zipWriter — deterministic and round-trippable", () => {
  const files = new Map();
  files.set(
    "manifest.json",
    Buffer.from(JSON.stringify({ a: 1, b: 2 }), "utf8"),
  );
  files.set("dist/index.js", Buffer.from("export const x = 1;", "utf8"));
  files.set(
    "dist/views/publish-panel.js",
    Buffer.from("export default 1;", "utf8"),
  );

  const a = buildKczZip(files, { deflateRawSync });
  const b = buildKczZip(files, { deflateRawSync });
  assert.deepEqual(a, b, "two builds of the same map must be byte-equal");

  // Find each local-file header and confirm the compressed payload
  // round-trips through inflate.
  let offset = 0;
  const seen = new Set();
  while (offset < a.length) {
    const sig = a.readUInt32LE(offset);
    if (sig !== 0x04034b50) break;
    const method = a.readUInt16LE(offset + 8);
    const compressedSize = a.readUInt32LE(offset + 18);
    const uncompressedSize = a.readUInt32LE(offset + 22);
    const nameLen = a.readUInt16LE(offset + 26);
    const extraLen = a.readUInt16LE(offset + 28);
    const name = a.toString("utf8", offset + 30, offset + 30 + nameLen);
    const payloadStart = offset + 30 + nameLen + extraLen;
    const payload = a.subarray(
      payloadStart,
      payloadStart + compressedSize,
    );
    const decoded = method === 8 ? inflateRawSync(payload) : payload;
    assert.equal(decoded.length, uncompressedSize, name);
    assert.equal(decoded.compare(files.get(name)), 0, name);
    seen.add(name);
    offset = payloadStart + compressedSize;
  }
  assert.equal(seen.size, files.size);
  assert.ok(seen.has("manifest.json"));
  assert.ok(seen.has("dist/index.js"));
  assert.ok(seen.has("dist/views/publish-panel.js"));
});

test("zipWriter — rejects non-Map input", () => {
  assert.throws(
    () => buildKczZip({}, { deflateRawSync }),
    /Map<string, Buffer>/,
  );
});
