import { describe, it, expect } from "vitest";
import { aec } from "../api/aec";

describe("aec IPC client (in-process fallback)", () => {
  it("creates a project and lists it in recents", async () => {
    const summary = await aec.project.createFromTemplate(
      "interior.apartment",
      "Test Apt",
    );
    expect(summary).toHaveProperty("projectId");
    expect((summary as { name: string }).name).toBe("Test Apt");
    const recents = await aec.project.listRecents();
    expect((recents as { projectId: string }[]).some((r) => r.projectId === (summary as { projectId: string }).projectId)).toBe(true);
  });

  it("returns the seeded asset library", async () => {
    const rows = await aec.design.listAssets({ tags: [], styleTags: [] });
    expect(Array.isArray(rows)).toBe(true);
    expect((rows as { assetId: string }[]).length).toBeGreaterThan(0);
  });

  it("returns the AI tool registry", async () => {
    const tools = await aec.ai.listTools();
    expect(Array.isArray(tools)).toBe(true);
    expect((tools as { id: string }[]).find((t) => t.id === "plan_detection")).toBeDefined();
  });

  it("returns a hardware profile", async () => {
    const s = await aec.runtime.status();
    const status = s as { tier: string; ramTotalMb: number };
    expect(["Low", "Medium", "High", "Pro"]).toContain(status.tier);
    expect(status.ramTotalMb).toBeGreaterThan(0);
  });
});
