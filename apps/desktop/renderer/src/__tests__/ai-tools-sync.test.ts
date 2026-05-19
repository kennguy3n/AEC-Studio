/**
 * Cross-language sync test for the AI tool catalogue.
 *
 * The Rust safety validator parses
 * `crates/aec_ai/data/ai_tools.json` at compile time. The TypeScript
 * bridge holds an in-memory mirror in `apps/desktop/electron/ai-tools.ts`.
 * This test reads the canonical JSON from disk and asserts every entry in
 * the TypeScript array matches it field-for-field — so drift between the
 * two languages is caught at `npm test` time, not in production.
 *
 * Companion test on the Rust side:
 * `crates/aec_ai/src/tool_schema.rs::tests::defaults_match_canonical_json`.
 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

import { AI_TOOLS } from "../../../electron/ai-tools";

interface CanonicalTool {
  id: string;
  display_name: string;
  description: string;
  allowed_scopes: string[];
  max_entities_modified: number;
  grammar_key: string;
}

interface CanonicalFile {
  version: number;
  tools: CanonicalTool[];
}

function loadCanonical(): CanonicalFile {
  // From this test file:
  //   apps/desktop/renderer/src/__tests__/ai-tools-sync.test.ts
  // Up to the workspace root requires 5 `..` segments, then we descend
  // into `crates/aec_ai/data/ai_tools.json`.
  const jsonPath = resolve(
    __dirname,
    "..",
    "..",
    "..",
    "..",
    "..",
    "crates",
    "aec_ai",
    "data",
    "ai_tools.json",
  );
  const raw = readFileSync(jsonPath, "utf-8");
  return JSON.parse(raw) as CanonicalFile;
}

describe("AI tool catalogue sync (TS ↔ Rust canonical JSON)", () => {
  const canonical = loadCanonical();

  it("declares schema version 1 so the test gets bumped on shape changes", () => {
    expect(canonical.version).toBe(1);
  });

  it("ships exactly the 11 tools the Rust safety validator expects", () => {
    expect(canonical.tools).toHaveLength(11);
    expect(AI_TOOLS).toHaveLength(canonical.tools.length);
  });

  it.each(
    // Pair each canonical tool with its TS counterpart so the per-row
    // diff is readable in test output.
    Object.values(
      canonical.tools.reduce<Record<string, CanonicalTool>>((acc, t) => {
        acc[t.id] = t;
        return acc;
      }, {}),
    ).map((t) => [t.id, t] as const),
  )("TS entry %s matches the canonical JSON", (id, tool) => {
    const tsEntry = AI_TOOLS.find((x) => x.id === id);
    expect(
      tsEntry,
      `AI_TOOLS is missing tool '${id}' present in ai_tools.json`,
    ).toBeDefined();
    if (!tsEntry) return;

    // `scope` is the comma-joined form of `allowed_scopes`.
    expect(
      tsEntry.scope.split(","),
      `${id}: scopes drifted between TS and canonical JSON`,
    ).toEqual(tool.allowed_scopes);

    expect(
      tsEntry.maxEntitiesModified,
      `${id}: maxEntitiesModified drifted between TS and canonical JSON`,
    ).toBe(tool.max_entities_modified);

    expect(
      tsEntry.description,
      `${id}: description drifted between TS and canonical JSON`,
    ).toBe(tool.description);
  });

  it("never re-uses a tool id (both arrays must have unique ids)", () => {
    const tsIds = AI_TOOLS.map((t) => t.id);
    const canonicalIds = canonical.tools.map((t) => t.id);
    expect(new Set(tsIds).size).toBe(tsIds.length);
    expect(new Set(canonicalIds).size).toBe(canonicalIds.length);
  });
});
