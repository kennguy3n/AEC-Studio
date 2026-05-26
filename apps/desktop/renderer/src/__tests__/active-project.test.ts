import { afterEach, describe, expect, it } from "vitest";
import {
  clearActiveProjectPath,
  getActiveProjectPath,
  peekActiveProjectPath,
  setActiveProjectPath,
} from "../../../electron/active-project";

/**
 * The active-project tracker is the main-process side of the
 * renderer / bridge handshake. The renderer's `aec.draft.*` and
 * `aec.deliver.*` APIs deliberately don't carry a `projectPath` on
 * every call — pages know which project is open at most via their
 * own state, but the Rust bridge is per-project and needs the path
 * on every call.
 *
 * The IPC layer in `electron/ipc.ts` reads this tracker when
 * forwarding a no-path call into the bridge; it's set by
 * `project:open` / `project:createFromTemplate` immediately after
 * the bridge succeeds. These tests pin the contract:
 *   1. set + get round-trips a non-empty string;
 *   2. get throws a descriptive error when no project is open;
 *   3. peek returns null instead of throwing;
 *   4. clear resets the slot;
 *   5. set rejects empty strings.
 */

afterEach(() => {
  clearActiveProjectPath();
});

describe("active-project", () => {
  it("round-trips a project path", () => {
    setActiveProjectPath("/projects/demo.aecstudio");
    expect(peekActiveProjectPath()).toBe("/projects/demo.aecstudio");
    expect(getActiveProjectPath("test")).toBe("/projects/demo.aecstudio");
  });

  it("getActiveProjectPath throws with the method name when unset", () => {
    expect(() => getActiveProjectPath("deliverCreateRevision")).toThrow(
      /deliverCreateRevision: no project is currently open/,
    );
  });

  it("peekActiveProjectPath returns null when unset", () => {
    expect(peekActiveProjectPath()).toBeNull();
  });

  it("clearActiveProjectPath resets the slot", () => {
    setActiveProjectPath("/tmp/a.aecstudio");
    clearActiveProjectPath();
    expect(peekActiveProjectPath()).toBeNull();
  });

  it("setActiveProjectPath rejects empty strings", () => {
    expect(() => setActiveProjectPath("")).toThrow(/non-empty/);
  });

  it("setActiveProjectPath overwrites the previous value", () => {
    setActiveProjectPath("/projects/a.aecstudio");
    setActiveProjectPath("/projects/b.aecstudio");
    expect(peekActiveProjectPath()).toBe("/projects/b.aecstudio");
  });
});
