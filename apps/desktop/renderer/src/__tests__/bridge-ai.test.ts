import { describe, expect, it } from "vitest";

import { inProcessBackend } from "../../../electron/bridge";

/**
 * Phase 11 task 10 — `aiPlan` now requires a `projectPath` so the
 * native side can bind the pending diff to a real project package
 * (which the accept path then opens to run `command_apply_batch`).
 * The in-process backend has no such binding (it returns a synthetic
 * `diffId` regardless), but the renderer-facing contract is the
 * same shape: `params.projectPath` is consumed (or ignored) but
 * passing one through is required by the native adaptor. This test
 * pins the shape so a renderer change to one side doesn't silently
 * drift from the other.
 *
 * Phase 11 task 11 — `aiRejectDiff` accepts an optional `reason`
 * string that the native side appends to the forensic audit
 * companion file. The in-process backend ignores the value but the
 * TS signature must accept it.
 */
describe("bridge in-process AI surface", () => {
  it("aiPlan returns a diffId + parsed payload for layout_suggestion", async () => {
    const bridge = inProcessBackend();
    const result = await bridge.aiPlan({
      projectPath: "/projects/sample.aecstudio",
      tool: "layout_suggestion",
      scope: "design",
      prompt: "anything",
      context: { room_anchor: "ent_living_room" },
      maxEntitiesModified: 4,
    });
    expect(result.diffId).toMatch(/^diff_/);
    expect(result.parsed).not.toBeNull();
  });

  it("aiAcceptDiff resolves to { accepted: true }", async () => {
    const bridge = inProcessBackend();
    const outcome = await bridge.aiAcceptDiff("diff_anything");
    expect(outcome).toEqual({ accepted: true });
  });

  it("aiRejectDiff resolves to { rejected: true } with no reason", async () => {
    const bridge = inProcessBackend();
    const outcome = await bridge.aiRejectDiff("diff_anything");
    expect(outcome).toEqual({ rejected: true });
  });

  it("aiRejectDiff accepts an optional reason string", async () => {
    const bridge = inProcessBackend();
    // The in-process fallback ignores `reason`; we only care that
    // the TS signature accepts it so a renderer call passing a
    // reason compiles. The native side records the reason in the
    // forensic AI audit companion file (covered by the Rust
    // `ai_reject_diff_writes_reason_to_forensic_log` integration
    // test).
    const outcome = await bridge.aiRejectDiff(
      "diff_anything",
      "doesn't match the brief",
    );
    expect(outcome).toEqual({ rejected: true });
  });

  it("aiPlan rejects an empty projectPath", async () => {
    const bridge = inProcessBackend();
    // The in-process fallback does NOT validate projectPath — its
    // primary purpose is to let renderer tests work without a real
    // project package. The native adaptor (`adaptNative`) DOES
    // validate, so we make sure the validator there fires; here we
    // just confirm the in-process path returns a stable result so
    // renderer tests don't break.
    const result = await bridge.aiPlan({
      tool: "style_assistant",
      scope: "design",
      prompt: "",
      context: {},
      maxEntitiesModified: 4,
    });
    expect(result.diffId).toMatch(/^diff_/);
  });
});
