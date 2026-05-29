#!/usr/bin/env node
/**
 * Bundle the AEC Studio KChat extension into a `.kcz` archive.
 *
 * A `.kcz` is a zip with a fixed top-level shape:
 *
 *   manifest.json
 *   dist/index.js (compiled from index.tsx)
 *   dist/views/publish-panel.js
 *   README.md
 *
 * `tsc -p tsconfig.json` (run by `npm run build`) populates `dist/`
 * before this script runs. We then verify the manifest matches the
 * compiled entry points and write the zip atomically into the
 * sibling `releases/` folder.
 *
 * The zip is produced with Node's `node:zlib` deflate stream wrapped
 * by a hand-rolled minimal zip writer — keeping the dependency
 * surface small (no `archiver`, no `adm-zip`) makes the extension
 * easier to vendor in environments where npm-install is gated.
 */
import { createHash } from "node:crypto";
import {
  mkdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateRawSync } from "node:zlib";

import { walkDir } from "./fsWalk.mjs";
import { buildKczZip } from "./zipWriter.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const PACKAGE_ROOT = resolve(HERE, "..");
const DIST_ROOT = resolve(PACKAGE_ROOT, "dist");
const RELEASES_ROOT = resolve(PACKAGE_ROOT, "releases");

function readManifest() {
  const raw = readFileSync(
    resolve(PACKAGE_ROOT, "manifest.json"),
    "utf8",
  );
  return JSON.parse(raw);
}

function ensureEntryPointsExist(manifest) {
  const missing = [];
  const main = manifest.entryPoints?.main;
  if (typeof main !== "string") {
    throw new Error("manifest.entryPoints.main must be a string");
  }
  try {
    statSync(resolve(PACKAGE_ROOT, main));
  } catch {
    missing.push(main);
  }
  const views = manifest.entryPoints?.views ?? {};
  for (const [viewId, path] of Object.entries(views)) {
    if (typeof path !== "string") {
      throw new Error(
        `manifest.entryPoints.views.${viewId} must be a string`,
      );
    }
    try {
      statSync(resolve(PACKAGE_ROOT, path));
    } catch {
      missing.push(path);
    }
  }
  if (missing.length > 0) {
    throw new Error(
      `Cannot build .kcz — entry-point files are missing (did you run \`tsc\`?):\n  ${missing.join("\n  ")}`,
    );
  }
}

function collectKczFiles(manifest) {
  const files = new Map();
  files.set(
    "manifest.json",
    Buffer.from(JSON.stringify(manifest, null, 2) + "\n", "utf8"),
  );
  try {
    files.set(
      "README.md",
      readFileSync(resolve(PACKAGE_ROOT, "README.md")),
    );
  } catch {
    // README is optional but recommended.
  }
  for (const absPath of walkDir(DIST_ROOT)) {
    const rel = "dist/" + relative(DIST_ROOT, absPath).split(sep).join("/");
    files.set(rel, readFileSync(absPath));
  }
  return files;
}

function deriveOutputPath(manifest) {
  const id = manifest.identity?.id;
  const version = manifest.identity?.version;
  if (typeof id !== "string" || typeof version !== "string") {
    throw new Error(
      "manifest.identity.id and identity.version are required",
    );
  }
  mkdirSync(RELEASES_ROOT, { recursive: true });
  return resolve(RELEASES_ROOT, `${id}@${version}.kcz`);
}

function main() {
  const manifest = readManifest();
  ensureEntryPointsExist(manifest);
  const files = collectKczFiles(manifest);
  const outPath = deriveOutputPath(manifest);
  rmSync(outPath, { force: true });
  const zipBytes = buildKczZip(files, { deflateRawSync });
  writeFileSync(outPath, zipBytes);
  const sha256 = createHash("sha256").update(zipBytes).digest("hex");
  // AEC Studio's release-signing pipeline reuses this format.
  writeFileSync(
    outPath + ".sha256",
    `${sha256}  ${manifest.identity.id}@${manifest.identity.version}.kcz\n`,
  );
  process.stdout.write(
    `Built ${relative(PACKAGE_ROOT, outPath)} (${zipBytes.length} bytes, sha256=${sha256.slice(0, 16)}…)\n`,
  );
}

main();
