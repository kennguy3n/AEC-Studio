/**
 * Single source of truth for the **TypeScript** view of the AI tool
 * catalogue.
 *
 * Every entry here mirrors `crates/aec_ai/data/ai_tools.json`, which is
 * the canonical JSON file that the Rust safety validator
 * (`crates/aec_ai/src/tool_schema.rs::ToolSchemaRegistry::defaults`)
 * parses at compile time. Both the Electron-side `aiListTools` handler
 * (in `electron/bridge.ts`) and the renderer-side Vitest fallback
 * (in `renderer/src/api/renderer-backend.ts`) re-export this array, so
 * there is exactly one place to edit when adding or changing a tool's
 * safety envelope.
 *
 * Drift between this file and the canonical JSON is caught by the
 * cross-language sync test at
 * `renderer/src/__tests__/ai-tools-sync.test.ts`, which reads the JSON
 * from disk and asserts every entry matches `id`, `scope`,
 * `maxEntitiesModified`, and `description`. The Rust half of the same
 * contract is enforced by
 * `tool_schema::tests::defaults_match_canonical_json`.
 *
 * The `scope` field is a comma-joined list of the Rust `Scope` enum
 * variants the tool is permitted to act in — exactly the values the
 * validator's `Scope` deserializer expects, e.g. `"design,draft"`.
 */
export interface AiTool {
  /** Stable tool identifier (matches Rust `ToolName::as_str`). */
  id: string;
  /** Comma-joined list of allowed scopes, e.g. `"design,draft"`. */
  scope: string;
  /** Upper bound on entities a single invocation may modify. */
  maxEntitiesModified: number;
  /** Human-readable description shown in the AI tool picker. */
  description: string;
}

export const AI_TOOLS: AiTool[] = [
  {
    id: "plan_detection",
    scope: "design,draft",
    maxEntitiesModified: 64,
    description: "Detect walls and openings from imported plan.",
  },
  {
    id: "plan_to_wall",
    scope: "design,draft",
    maxEntitiesModified: 64,
    description: "Convert a detected plan into parametric walls.",
  },
  {
    id: "style_assistant",
    scope: "design",
    maxEntitiesModified: 24,
    description: "Propose furniture and finishes for a given style brief.",
  },
  {
    id: "layout_suggestion",
    scope: "design",
    maxEntitiesModified: 16,
    description: "Suggest a furniture layout for a room shape.",
  },
  {
    id: "render_doctor",
    scope: "render",
    maxEntitiesModified: 8,
    description: "Diagnose noise, exposure, and lighting in a render.",
  },
  {
    id: "cad_cleanup",
    scope: "draft",
    maxEntitiesModified: 256,
    description: "Clean and rationalise an imported drawing.",
  },
  {
    id: "schedule_fill",
    scope: "bim,deliver",
    maxEntitiesModified: 128,
    description: "Fill BIM property schedules.",
  },
  {
    id: "classification",
    scope: "bim",
    maxEntitiesModified: 128,
    description: "Classify imported geometry into IFC entities.",
  },
  {
    id: "property_fill",
    scope: "bim",
    maxEntitiesModified: 128,
    description: "Populate property sets on classified elements.",
  },
  {
    id: "validation_help",
    scope: "bim",
    maxEntitiesModified: 64,
    description: "Explain BIM validation findings.",
  },
  {
    id: "cover_page_draft",
    scope: "deliver",
    maxEntitiesModified: 4,
    description: "Draft a proposal pack cover page.",
  },
];
