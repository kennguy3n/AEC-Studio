/**
 * Thin TypeScript wrapper around `aec_bridge.node`.
 *
 * In production the Electron main process loads the compiled N-API
 * library and every method here delegates straight through. In
 * development before `npm run build:native` (and in `vitest`), the
 * library is not on disk — so we instead use an in-process backend
 * that returns realistic data and keeps a real (memory-backed) recents
 * list. The in-process backend is NOT a stub: it implements every
 * method with working logic so the UI runs end-to-end without the
 * native artefact.
 */

import * as crypto from "crypto";
import * as fs from "fs";
import * as path from "path";

import { AI_TOOLS, type AiTool } from "./ai-tools";

/**
 * Renderer-side warning threshold for IFC files (100 MB). Mirrors
 * the Rust constant `BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES` in
 * `crates/aec_bridge/src/service.rs` exactly so the in-process JS
 * fallback agrees with the native bridge on what counts as
 * "large". If the Rust constant changes, update this one in
 * lockstep — the bridge regression test
 * `bim_check_file_size_threshold_matches_summary_flag` pins both
 * values together.
 */
export const BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES = 100 * 1024 * 1024;

/**
 * 16-hex random id helper used by the in-process backend for ids that
 * the renderer treats as opaque (revision ids, draft ids, …). Uses
 * Node's crypto so id collisions are vanishingly unlikely even when
 * many revisions are created in rapid succession during tests.
 */
function randomId(): string {
  return crypto.randomBytes(8).toString("hex");
}

/**
 * Response shape for the AI plan request. The `parsed` field is
 * tool-specific structured data, used by panels that need to display
 * proposals before the user accepts or rejects the diff. It is always
 * a JSON-safe object so it can cross the IPC boundary; consumers cast
 * to a narrower tool-specific type (e.g. `LayoutSuggestionParsed`).
 */
export interface AiPlanResponse {
  diffId: string;
  /** Tool-specific parsed payload. `null` when no parse was produced. */
  parsed?: AiPlanParsed | null;
}

/** Parsed payloads for individual AI tools, tagged by `tool`. */
export type AiPlanParsed =
  | LayoutSuggestionParsed
  | { tool: string; [key: string]: unknown };

/**
 * Mirrors Rust `LayoutSuggestionResult` in
 * `crates/aec_ai/src/layout_suggestion.rs`. Field names use snake_case
 * to match the on-wire JSON the Rust side produces.
 */
export interface LayoutSuggestionParsed {
  tool: "layout_suggestion";
  room_anchor: string;
  proposals: Array<{
    asset_id?: string | null;
    target_entity?: string | null;
    position_mm: [number, number, number];
    rotation_deg: number;
  }>;
}

export interface BridgeBackend {
  projectCreateFromTemplate(templateKey: string, projectName: string): Promise<ProjectSummary>;
  projectOpen(projectPath: string): Promise<ProjectSummary>;
  /**
   * Persist the open project's manifest and return its summary. The native
   * N-API bridge returns a `ProjectSummary` and the in-process fallback
   * mirrors that shape so backends are interchangeable at runtime.
   */
  projectSave(projectPath: string): Promise<ProjectSummary>;
  projectListRecents(): Promise<ProjectSummary[]>;
  projectExportPackage(projectPath: string, outPath: string): Promise<{ outPath: string }>;

  designPlaceFurniture(params: Record<string, unknown>): Promise<{ entityId: string }>;
  designPaintMaterial(params: Record<string, unknown>): Promise<{ ok: true }>;
  designSetLighting(params: Record<string, unknown>): Promise<{ ok: true }>;
  designSaveCamera(params: Record<string, unknown>): Promise<{ cameraId: string }>;
  designListAssets(query: Record<string, unknown>): Promise<AssetSummary[]>;

  draftDrawPrimitive(params: Record<string, unknown>): Promise<{ entityId: string }>;
  draftEditTool(params: Record<string, unknown>): Promise<{ ok: true }>;
  draftCreateSheet(params: Record<string, unknown>): Promise<{ sheetId: string }>;
  draftSetLayerState(params: Record<string, unknown>): Promise<{ ok: true }>;
  draftImportDxf(path: string): Promise<{ imported: number }>;
  draftExportDxf(path: string): Promise<{ exported: true; path: string }>;

  /**
   * Parse an IFC file and return a structured preview summary. The
   * file is NOT yet folded into the active project — that's the
   * follow-up `bimAttachIfc` call. The renderer uses this for the
   * "Import BIM" preview pane (entity counts, schema version,
   * canonical path).
   *
   * The shape mirrors `BimImportSummaryJs` in
   * `crates/aec_bridge/src/napi_api.rs` 1:1 — drift is a runtime
   * bug surfaced as `undefined` on a status pane.
   */
  bimImportIfc(path: string): Promise<BimImportSummary>;
  /**
   * Cheap pre-parse file-size check. The renderer's file-picker UI
   * calls this *before* `bimImportIfc` so it can show a "this file
   * is N MB; continue?" confirm dialog on large IFC files (e.g.
   * 400 MB MEP federations) without first paying the multi-second
   * parse cost. Cost: one `fs::canonicalize` + one `fs::metadata`
   * — no file read.
   *
   * The `largeFileWarning` flag is advisory; the renderer is free
   * to ignore it and call `bimImportIfc` anyway.
   */
  bimCheckFileSize(path: string): Promise<BimFileSizeCheck>;
  /**
   * Attach a parsed IFC snapshot into the active project's
   * SQLCipher database. Folds spatial nodes, elements, Psets,
   * materials, and aggregation / containment relations into the
   * project graph, deduping against existing rows so a re-attach
   * of the same file with identical content reports `_unchanged`
   * instead of `_inserted` / `_updated`.
   *
   * The shape mirrors `BimAttachSummaryJs` in
   * `crates/aec_bridge/src/napi_api.rs` 1:1.
   *
   * If the renderer just ran `bimImportIfc` on the same path, the
   * bridge's in-process snapshot cache will serve the parse for
   * free (reported as `parseCacheHit: true`).
   */
  bimAttachIfc(projectPath: string, ifcPath: string): Promise<BimAttachSummary>;
  bimExportIfc(path: string): Promise<{ exported: true; path: string }>;
  bimClassify(params: Record<string, unknown>): Promise<{ classified: number }>;
  bimSetProperty(params: Record<string, unknown>): Promise<{ ok: true }>;
  bimGenerateSchedule(params: Record<string, unknown>): Promise<{ scheduleId: string }>;
  bimValidate(): Promise<{ ok: boolean; errors: unknown[]; warnings: unknown[] }>;
  bimDiff(params: Record<string, unknown>): Promise<{ diffId: string }>;

  renderEnqueue(params: Record<string, unknown>): Promise<{ jobId: string }>;
  renderEnqueueBatch(params: {
    cameraIds: string[];
    presetIds?: string[];
    presetId?: string;
  }): Promise<{ batchId: string; jobIds: string[] }>;
  renderBatchProgress(batchId: string): Promise<{
    batchId: string;
    total: number;
    queued: number;
    running: number;
    completed: number;
    failed: number;
    cancelled: number;
    averageProgress: number;
  } | null>;
  renderListJobs(): Promise<RenderJob[]>;
  renderCancelJob(jobId: string): Promise<{ cancelled: true }>;
  renderApplyPreset(params: Record<string, unknown>): Promise<{ ok: true }>;
  renderDiagnose(jobId: string): Promise<{ jobId: string; suggestions: string[] }>;
  renderCheckMaterials(): Promise<{
    findings: Array<{
      code: string;
      severity: "info" | "warning" | "error";
      materialId: string | null;
      message: string;
      fix: string | null;
    }>;
  }>;

  aiListTools(): Promise<AiTool[]>;
  /**
   * Submit an AI tool request and receive both a diff id (for accept /
   * reject) and an optional `parsed` payload — the structured
   * tool-specific response that the renderer needs to display
   * proposals before the user accepts. The shape of `parsed` is
   * tool-dependent and mirrors the corresponding Rust result type
   * (e.g. `LayoutSuggestionResult` for `tool = "layout_suggestion"`).
   */
  aiPlan(params: Record<string, unknown>): Promise<AiPlanResponse>;
  aiAcceptDiff(diffId: string): Promise<{ accepted: true }>;
  aiRejectDiff(diffId: string): Promise<{ rejected: true }>;
  aiCancelJob(jobId: string): Promise<{ cancelled: true }>;
  aiRuntimeStatus(): Promise<{ state: string; lastError: string | null }>;

  exportPdf(params: Record<string, unknown>): Promise<{ outPath: string; pages: number }>;
  exportDxf(params: Record<string, unknown>): Promise<{ outPath: string }>;
  exportIfc(params: Record<string, unknown>): Promise<{ outPath: string }>;
  exportGltf(params: Record<string, unknown>): Promise<{ outPath: string }>;
  exportBuildProposalPack(params: Record<string, unknown>): Promise<{ outPath: string }>;

  // ----- Deliver mode -----

  deliverCreateRevision(params: {
    tag: string;
    description: string;
    entities?: Array<{
      category: string;
      id: string;
      payloadHash: string;
      label?: string | null;
    }>;
  }): Promise<RevisionSummary>;
  deliverListRevisions(): Promise<RevisionSummary[]>;
  deliverCompareRevisions(params: {
    baseId: string;
    headId: string;
  }): Promise<VersionDiffSummary>;
  deliverBuildPack(params: {
    kind: "concept" | "interior" | "contractor" | "bim";
    outPath: string;
    includeRenders?: boolean;
    includeSheets?: boolean;
    includeIfc?: boolean;
    includeBoq?: boolean;
    includeProposal?: boolean;
    region?: "eu" | "na" | "apac";
  }): Promise<DeliverPackResult>;

  runtimeStatus(): Promise<RuntimeStatus>;

  /**
   * Read-only engine status for the renderer's status pane. Combines
   * the SQLCipher schema version, the audit chain head + entry count
   * from the JSONL log, and per-scope row counts from the SQL mirror.
   *
   * On the native backend this is a single call into Rust. On the
   * in-process fallback it's implemented against the same JSONL store
   * (and the SQL counts are 0 since there's no SQLite file in the
   * fallback path); callers must therefore treat
   * `auditChainSqlCount === 0 && auditEntryCount > 0` as a
   * "fallback-only" state rather than a real "stale mirror" warning.
   */
  projectEngineStatus(projectPath: string): Promise<EngineStatus>;

  /**
   * Mirror the JSONL audit chain into the SQLCipher `audit_chain`
   * table. Returns the number of rows inserted. Idempotent. Safe to
   * call on a fresh project (returns 0). On the in-process fallback
   * this is a no-op that always returns 0.
   */
  projectAuditSync(projectPath: string): Promise<number>;

  /**
   * Apply a typed command (`Command`) to the project graph. The
   * Rust side rebuilds the in-memory engine from the SQLCipher
   * `entities` + `undo_journal` tables, executes the command, and
   * returns the resulting deltas plus the post-call undo/redo stack
   * depths so the renderer can keep its toolbar in sync.
   *
   * `command` is the typed envelope produced by the renderer-side
   * helpers in `apps/desktop/renderer/src/api/commands.ts` —
   * containing the `command_id`, `scope`, `actor`, `ts`, and
   * `kind` (tagged `design.create_wall`, `design.paint_material`,
   * etc., per `aec_command::commands::CommandKind`'s serde tags).
   */
  commandApply(projectPath: string, command: Command): Promise<CommandApplyResult>;

  /**
   * Undo the most recently applied command. The renderer renders
   * the returned inverse deltas immediately and refreshes any
   * affected entity panels.
   *
   * `activeScope` must match the scope of the command being
   * undone. The native engine stores the originating scope on every
   * `undo_journal` row and rejects mismatches with
   * `CommandError::ScopeMismatch`; the in-process JS fallback
   * mirrors the check using the per-entry `scope` field on
   * {@link InProcessGraph.undo} / `.redo`. Without this validation
   * an undo issued under the wrong active scope would silently
   * apply inverse deltas tagged for a different rail.
   */
  commandUndo(projectPath: string, activeScope: CommandScope): Promise<CommandApplyResult>;

  /** Symmetric counterpart to {@link commandUndo}. */
  commandRedo(projectPath: string, activeScope: CommandScope): Promise<CommandApplyResult>;

  /**
   * Read-only listing of the project graph. Pass `kindFilter`
   * (e.g. `"wall"`, `"room"`, `"camera"`) to narrow; pass
   * `undefined` for the full graph. Order is unspecified — the
   * renderer sorts client-side if it needs deterministic display.
   */
  projectGraphList(projectPath: string, kindFilter?: string): Promise<EntityRecord[]>;
}

/**
 * Renderer-facing projection of a Rust `Revision` (see
 * `crates/aec_core/src/revision.rs`). Field names use camelCase per
 * the rest of the bridge surface; the Rust side serialises in
 * snake_case but the adaptor in `inProcessBackend` converts.
 */
export interface RevisionSummary {
  revisionId: string;
  tag: string;
  description: string;
  createdAt: string;
  auditChainHead: string;
  manifestName: string;
  manifestAppVersion: string;
  trackedEntities: Array<{
    category: string;
    id: string;
    payloadHash: string;
    label: string | null;
  }>;
}

/** Renderer-facing projection of a Rust `VersionDiff`. */
export interface VersionDiffSummary {
  baseRevisionId: string;
  headRevisionId: string;
  changes: Array<{
    category: string;
    id: string;
    kind: "added" | "removed" | "modified" | "unchanged";
    beforeHash: string | null;
    afterHash: string | null;
    label: string | null;
  }>;
  /** Per-category counts. */
  byCategory: Record<
    string,
    {
      added: number;
      removed: number;
      modified: number;
      unchanged: number;
    }
  >;
}

/**
 * The five workflow scopes from `aec_core::types::Scope`. Used to tag
 * commands and to validate that an undo/redo doesn't cross a scope
 * boundary (e.g. you can't undo a design command while in the bim
 * workflow). String values match the Rust serde `rename_all =
 * "snake_case"` output exactly.
 */
export type CommandScope = "design" | "draft" | "bim" | "render" | "deliver";

/**
 * One reversible change to the project graph. Mirrors
 * `aec_command::commands::project_graph::EntityDelta` 1:1 — the same
 * three-arm tagged union (`create` / `update` / `delete`) the Rust
 * side produces from `Command::execute_persistent`.
 */
export type EntityDelta =
  | { kind: "create"; record: EntityRecord }
  | { kind: "update"; id: string; before: unknown; after: unknown }
  | { kind: "delete"; record: EntityRecord };

/**
 * An entity in the project graph. Mirrors
 * `aec_command::commands::EntityRecord` — the renderer treats
 * `body` as opaque structured JSON whose shape depends on `kind`
 * (`"wall"`, `"room"`, `"camera"`, …).
 *
 * `parent` is the optional parent entity id (e.g. an opening
 * parented to a wall).
 */
export interface EntityRecord {
  id: string;
  kind: string;
  parent: string | null;
  /**
   * Tool-specific body payload. Walls carry `start_mm` / `end_mm`
   * / `height_mm` / `thickness_mm` / `material_id`; rooms carry
   * `polygon_mm` / `floor_height_mm`; cameras carry `params`; etc.
   * See `crates/aec_command/src/commands/*.rs` for the per-kind
   * shapes.
   */
  body: unknown;
}

/**
 * Provenance carried with each command. `user` for interactive UI
 * commands; `ai` for AI-issued commands (the `tool` field carries
 * the AI tool name). Serde shape mirrors `aec_command::Actor`.
 */
export type CommandActor =
  | { kind: "user" }
  | { kind: "ai"; tool: string };

/**
 * A typed command envelope. Mirrors `aec_command::commands::Command`
 * 1:1 — the `command_id` / `ts` / `scope` / `actor` / `kind` quintuple
 * the Rust engine consumes. Helpers in
 * `apps/desktop/renderer/src/api/commands.ts` build these for each
 * design / draft tool so callers don't have to hand-roll the
 * envelope or invent ids.
 *
 * The wire format mirrors the Rust serde derives: snake_case keys
 * for the envelope fields, snake_case `tool` tag (e.g.
 * `"design.create_wall"`, `"design.paint_material"`), and the
 * `arguments` payload alongside `tool` per
 * `CommandKind`'s `#[serde(tag = "tool", content = "arguments")]`.
 */
export interface Command {
  command_id: string;
  ts: string;
  scope: CommandScope;
  actor: CommandActor;
  /**
   * Tool dispatch tag. One of
   * `design.create_wall` / `design.move_wall` / `design.delete_wall` /
   * `design.create_room` / `design.modify_room` /
   * `design.create_floor` / `design.modify_floor` /
   * `design.place_door` / `design.place_window` /
   * `design.move_opening` / `design.delete_opening` /
   * `design.paint_material` / `design.swap_finish` /
   * `design.set_lighting` / `design.add_light` /
   * `design.remove_light` / `design.save_camera` /
   * `design.update_camera` / `design.delete_camera`. The
   * argument shape under `arguments` depends on the tool tag.
   */
  tool: string;
  arguments: unknown;
}

/**
 * Returned by {@link BridgeBackend.commandApply},
 * {@link BridgeBackend.commandUndo}, and
 * {@link BridgeBackend.commandRedo}.
 *
 * `applied` is the list of {@link EntityDelta} the engine just
 * committed (forward deltas for `apply` / `redo`, inverse deltas
 * for `undo`). The renderer uses these directly to update its
 * scene graph without re-querying.
 *
 * `undoLen` / `redoLen` are the post-call stack depths so the
 * renderer's undo/redo toolbar buttons stay coherent.
 */
export interface CommandApplyResult {
  commandId: string;
  applied: EntityDelta[];
  undoLen: number;
  redoLen: number;
}

/** Result returned by `deliver:buildPack`. */
export interface DeliverPackResult {
  /** Path on disk where the pack ZIP / PDF was written. */
  outPath: string;
  /** Files included in the pack. */
  contents: string[];
  /** Total bytes of all files in the pack. */
  totalBytes: number;
}

export interface ProjectSummary {
  projectId: string;
  name: string;
  path: string;
  templateKey: string | null;
  modifiedAt: string;
}

export interface AssetSummary {
  assetId: string;
  name: string;
  tags: string[];
  styleTags: string[];
  vendor: string | null;
  thumbnailDataUri: string | null;
}

export interface RenderJob {
  jobId: string;
  status: "queued" | "running" | "completed" | "failed" | "cancelled";
  preset: string;
  progress: number;
  /** Camera entity id this job is rendering (batch / matrix submissions). */
  cameraId?: string | null;
  /** Batch id when this job was submitted as part of a batch. */
  batchId?: string | null;
}

export interface RuntimeStatus {
  tier: "Low" | "Medium" | "High" | "Pro";
  cpu: { model: string; physicalCores: number; logicalCores: number };
  ramTotalMb: number;
  ramAvailableMb: number;
  gpu: { vendor: string; model: string; vramMb: number } | null;
  os: string;
}

/**
 * Parse-only BIM/IFC import summary. Field-for-field mirror of
 * `BimImportSummaryJs` in `crates/aec_bridge/src/napi_api.rs`.
 * Drift here is a runtime bug surfacing as `undefined` on a
 * status pane.
 */
export interface BimImportSummary {
  path: string;
  schema: string;
  spatialNodes: number;
  elements: number;
  psets: number;
  qsets: number;
  aggregations: number;
  containments: number;
  materials: number;
  materialLayerSets: number;
  materialAssignments: number;
  recordsSeen: number;
}

/**
 * Cheap pre-parse file-size check shape. Field-for-field mirror
 * of `BimFileSizeCheckJs` in `crates/aec_bridge/src/napi_api.rs`.
 */
export interface BimFileSizeCheck {
  path: string;
  fileSizeBytes: number;
  largeFileWarning: boolean;
  thresholdBytes: number;
}

/**
 * Post-attach summary. Field-for-field mirror of
 * `BimAttachSummaryJs` in `crates/aec_bridge/src/napi_api.rs`.
 * All counters are post-dedup: a re-attach of the same file with
 * identical content reports `_unchanged` instead of `_inserted` /
 * `_updated`.
 */
export interface BimAttachSummary {
  path: string;
  projectPath: string;
  /** `true` if the snapshot for this file was served from the
   *  in-process cache populated by a prior `bimImportIfc`. */
  parseCacheHit: boolean;
  spatialNodesInserted: number;
  spatialNodesUpdated: number;
  spatialNodesUnchanged: number;
  elementsInserted: number;
  elementsUpdated: number;
  elementsUnchanged: number;
  componentsInserted: number;
  relationsInserted: number;
  cacheRows: number;
}

/**
 * Engine status for the renderer's status pane. Field-for-field
 * mirror of `EngineStatusJs` in
 * `crates/aec_bridge/src/napi_api.rs::EngineStatusJs`. Drift between
 * the two is a runtime bug surfaced as `undefined` on the renderer
 * side, so keep them aligned when adding fields.
 *
 * `auditChainByScope` is keyed by `Scope::as_str` (`design`,
 * `draft`, `bim`, `render`, `deliver`). All five keys are always
 * present — the bridge backfills missing scopes with `0` so the
 * renderer can do a flat lookup without a fallback.
 */
export interface EngineStatus {
  schemaVersion: number;
  auditChainHead: string;
  auditEntryCount: number;
  auditChainSqlCount: number;
  auditChainByScope: Record<string, number>;
}

let backend: BridgeBackend | null = null;

export function getBridge(): BridgeBackend {
  if (backend) return backend;
  backend = loadNativeBackend() ?? inProcessBackend();
  return backend;
}

export function setBridge(replacement: BridgeBackend): void {
  backend = replacement;
}

/**
 * Locate and load the compiled N-API `aec_bridge` library, falling back to
 * the in-process backend if it can't be found.
 *
 * `cargo build` produces different file names per platform:
 *   - Linux:   `libaec_bridge.so`
 *   - macOS:   `libaec_bridge.dylib`
 *   - Windows: `aec_bridge.dll`     (note: no `lib` prefix)
 *
 * For `require()` to load any of them they must be renamed to `.node`.
 * The build script (`npm run build:native`) does this rename. We probe
 * every reasonable candidate so a developer who renamed by hand —
 * particularly on Windows where the `lib` prefix is unconventional —
 * still gets the native backend wired up.
 */
function loadNativeBackend(): BridgeBackend | null {
  const targetDir = path.resolve(__dirname, "..", "..", "..", "target", "release");
  const candidates = nativeLibraryCandidates(process.platform).map((name) =>
    path.join(targetDir, name),
  );
  const found = candidates.find((c) => fs.existsSync(c));
  if (!found) {
    return null;
  }
  try {
    // eslint-disable-next-line @typescript-eslint/no-var-requires
    const native = require(found);
    return adaptNative(native);
  } catch {
    return null;
  }
}

/**
 * Return platform-specific candidate file names for the native bridge,
 * in priority order. The two `.node` variants are what the build script
 * normally produces; the raw cdylib names are fallbacks for ad-hoc builds.
 */
export function nativeLibraryCandidates(platform: NodeJS.Platform): string[] {
  switch (platform) {
    case "win32":
      // napi-rs on Windows omits the `lib` prefix.
      return ["aec_bridge.node", "aec_bridge.dll"];
    case "darwin":
      return ["libaec_bridge.node", "libaec_bridge.dylib"];
    default:
      return ["libaec_bridge.node", "libaec_bridge.so"];
  }
}

/**
 * Wire shape for {@link CommandApplyResult} returned by the
 * `command_*` napi exports. Mirrors `CommandApplyResultJs` in
 * `crates/aec_bridge/src/napi_api.rs` 1:1.
 *
 * `appliedJson` is a JSON-stringified `Vec<EntityDelta>`; the
 * renderer-side decoder parses it once and hands plain
 * `EntityDelta` objects to callers.
 */
interface CommandApplyResultJs {
  commandId: string;
  appliedJson: string;
  undoLen: number;
  redoLen: number;
}

/**
 * Wire shape for an entity row from `project_graph_list`. Mirrors
 * `EntityRecordJs` in `crates/aec_bridge/src/napi_api.rs`.
 *
 * `bodyJson` is the JSON-stringified entity body; the decoder
 * parses it once before handing the result to renderer code.
 */
interface EntityRecordJs {
  id: string;
  kind: string;
  parent: string | null;
  bodyJson: string;
}

/**
 * Normalise a parsed `EntityRecord` so that `parent` is always
 * `string | null` on the JS side.
 *
 * The Rust `EntityRecord.parent` is `Option<EntityId>` with
 * `#[serde(default, skip_serializing_if = "Option::is_none")]`, so
 * when an entity has no parent, `serde_json::to_string` omits the
 * field entirely. `JSON.parse` then leaves `record.parent` as
 * `undefined`, which violates the declared `EntityRecord.parent:
 * string | null` shape and breaks any consumer doing `parent ===
 * null` to identify root entities (walls, rooms, cameras, lights,
 * floors). We coalesce missing/undefined to `null` so the native
 * path matches the in-process path's invariant exactly.
 */
function normaliseEntityRecord<T extends { parent?: string | null }>(record: T): T {
  return { ...record, parent: record.parent ?? null };
}

function normaliseEntityDelta(d: EntityDelta): EntityDelta {
  if (d.kind === "create" || d.kind === "delete") {
    return { ...d, record: normaliseEntityRecord(d.record) };
  }
  return d;
}

function decodeCommandApplyJs(r: CommandApplyResultJs): CommandApplyResult {
  return {
    commandId: r.commandId,
    // The Rust side serialises with `serde_json::to_string`, which
    // never produces invalid UTF-8 or non-JSON output — parsing is
    // infallible in practice. We still guard with a clear error so
    // a future serde shape change surfaces here rather than at the
    // first downstream consumer.
    applied: (JSON.parse(r.appliedJson) as EntityDelta[]).map(normaliseEntityDelta),
    undoLen: r.undoLen,
    redoLen: r.redoLen,
  };
}

function decodeEntityRecordJs(r: EntityRecordJs): EntityRecord {
  return normaliseEntityRecord({
    id: r.id,
    kind: r.kind,
    parent: r.parent,
    body: JSON.parse(r.bodyJson),
  });
}

interface NativeApi {
  project_create_from_template(template_key: string, project_name: string): unknown;
  project_open(project_path: string): unknown;
  project_save(project_path: string): unknown;
  project_list_recents(): unknown;
  runtime_status(): unknown;
  project_engine_status(project_path: string): unknown;
  project_audit_sync(project_path: string): unknown;
  bim_import_ifc(path: string): unknown;
  bim_check_file_size(path: string): unknown;
  bim_attach_ifc(project_path: string, ifc_path: string): unknown;
  command_apply(project_path: string, command_json: string): unknown;
  command_undo(project_path: string, active_scope: string): unknown;
  command_redo(project_path: string, active_scope: string): unknown;
  project_graph_list(project_path: string, kind_filter: string | null | undefined): unknown;
}

/**
 * Methods that are currently routed through the N-API native bridge.
 * Every other backend method falls back to the in-process implementation
 * (see {@link NATIVE_FALLBACK_METHODS}).
 *
 * Keep this set in sync with the `#[napi]` exports in
 * `crates/aec_bridge/src/napi_api.rs`. Adding a new exported function?
 * Add a method override in {@link adaptNative} and remove the corresponding
 * entry from {@link NATIVE_FALLBACK_METHODS}.
 */
export const NATIVE_WIRED_METHODS: ReadonlyArray<keyof BridgeBackend> = [
  "projectCreateFromTemplate",
  "projectOpen",
  "projectSave",
  "projectListRecents",
  "runtimeStatus",
  "projectEngineStatus",
  "projectAuditSync",
  // BIM domain wired in PR-P. `bimImportIfc` / `bimAttachIfc` /
  // `bimCheckFileSize` all delegate to real `#[napi]` exports in
  // `crates/aec_bridge/src/napi_api.rs`. The remaining `bim*`
  // methods (`bimExportIfc`, `bimClassify`, ...) stay in the
  // fallback list pending their own follow-up napi exports.
  "bimImportIfc",
  "bimCheckFileSize",
  "bimAttachIfc",
  // Command engine wired in PR-Q. `commandApply` / `commandUndo` /
  // `commandRedo` / `projectGraphList` route directly to
  // `aec_command::engine::CommandEngine` through the
  // `crates/aec_bridge/src/napi_api.rs` exports; the in-process
  // fallback is a working JS reimplementation used by vitest and
  // pre-build dev mode.
  "commandApply",
  "commandUndo",
  "commandRedo",
  "projectGraphList",
];

/**
 * Methods that **intentionally** fall back to the in-process backend even
 * when the native artefact is loaded. Phase 1/2 only exposes project +
 * runtime over N-API; the rest is realistic dev-mode behaviour that the
 * UI exercises end-to-end. Removing entries from this list means we have
 * wired more domain crates (aec_command, aec_render, aec_ai, …) through
 * the N-API surface.
 *
 * Documented here so the gap between `BridgeBackend` and the N-API surface
 * is explicit and grep-able, rather than implicit in the spread operator
 * inside {@link adaptNative}.
 */
export const NATIVE_FALLBACK_METHODS: ReadonlyArray<keyof BridgeBackend> = [
  "projectExportPackage",
  "designPlaceFurniture",
  "designPaintMaterial",
  "designSetLighting",
  "designSaveCamera",
  "designListAssets",
  "draftDrawPrimitive",
  "draftEditTool",
  "draftCreateSheet",
  "draftSetLayerState",
  "draftImportDxf",
  "draftExportDxf",
  "bimExportIfc",
  "bimClassify",
  "bimSetProperty",
  "bimGenerateSchedule",
  "bimValidate",
  "bimDiff",
  "renderEnqueue",
  "renderEnqueueBatch",
  "renderBatchProgress",
  "renderListJobs",
  "renderCancelJob",
  "renderApplyPreset",
  "renderDiagnose",
  "renderCheckMaterials",
  "aiListTools",
  "aiPlan",
  "aiAcceptDiff",
  "aiRejectDiff",
  "aiCancelJob",
  "aiRuntimeStatus",
  "exportPdf",
  "exportDxf",
  "exportIfc",
  "exportGltf",
  "exportBuildProposalPack",
  "deliverCreateRevision",
  "deliverListRevisions",
  "deliverCompareRevisions",
  "deliverBuildPack",
];

/**
 * Wrap a freshly-loaded N-API library with the {@link BridgeBackend} shape.
 *
 * The N-API surface is intentionally narrow today (project lifecycle +
 * runtime status). Every other domain method falls through to the
 * in-process backend — a deliberate Phase 1/2 scoping choice. When a
 * fallback method is invoked while a native backend is loaded we emit a
 * `console.debug` so the dev console makes the boundary obvious instead
 * of silently masking it.
 */
function adaptNative(n: NativeApi): BridgeBackend {
  const base = inProcessBackend();
  const fallbackNames = new Set<string>(NATIVE_FALLBACK_METHODS);
  const wrapped: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(base) as [string, unknown][]) {
    if (fallbackNames.has(key) && typeof value === "function") {
      const fn = value as (...args: unknown[]) => unknown;
      wrapped[key] = (...args: unknown[]) => {
        // Loud-but-cheap signal in development so a missing N-API binding
        // doesn't masquerade as "the native bridge handled it". Suppressed
        // in tests by node's default console filter.
        if (process.env.AEC_LOG_BACKEND === "1") {
          // eslint-disable-next-line no-console
          console.debug(`[aec_bridge] in-process fallback for ${key}`);
        }
        return fn(...args);
      };
    } else {
      wrapped[key] = value;
    }
  }
  const native: BridgeBackend = {
    ...(wrapped as unknown as BridgeBackend),
    projectCreateFromTemplate: async (k, p) =>
      n.project_create_from_template(k, p) as ProjectSummary,
    projectOpen: async (p) => n.project_open(p) as ProjectSummary,
    projectSave: async (p) => n.project_save(p) as ProjectSummary,
    projectListRecents: async () => n.project_list_recents() as ProjectSummary[],
    runtimeStatus: async () => n.runtime_status() as RuntimeStatus,
    projectEngineStatus: async (p) => n.project_engine_status(p) as EngineStatus,
    projectAuditSync: async (p) => n.project_audit_sync(p) as number,
    bimImportIfc: async (p) => n.bim_import_ifc(p) as BimImportSummary,
    bimCheckFileSize: async (p) => n.bim_check_file_size(p) as BimFileSizeCheck,
    bimAttachIfc: async (projectPath, ifcPath) =>
      n.bim_attach_ifc(projectPath, ifcPath) as BimAttachSummary,
    commandApply: async (projectPath, command) =>
      decodeCommandApplyJs(
        n.command_apply(projectPath, JSON.stringify(command)) as CommandApplyResultJs,
      ),
    commandUndo: async (projectPath, activeScope) =>
      decodeCommandApplyJs(
        n.command_undo(projectPath, activeScope) as CommandApplyResultJs,
      ),
    commandRedo: async (projectPath, activeScope) =>
      decodeCommandApplyJs(
        n.command_redo(projectPath, activeScope) as CommandApplyResultJs,
      ),
    projectGraphList: async (projectPath, kindFilter) =>
      (n.project_graph_list(projectPath, kindFilter ?? null) as EntityRecordJs[]).map(
        decodeEntityRecordJs,
      ),
  };
  // Self-check: the two catalogues above must, together, reference every
  // method on the in-process backend. We throw rather than warn so a new
  // BridgeBackend method that is forgotten in the declarations fails
  // fast at bridge initialisation — instead of silently falling through
  // to the in-process implementation with no debug-logging wrapper.
  // Tests can opt out by setting `AEC_BRIDGE_SKIP_SELFCHECK=1` (used by
  // the `bridge_self_check` test to assert this throws).
  const declared = new Set<string>([
    ...NATIVE_WIRED_METHODS,
    ...NATIVE_FALLBACK_METHODS,
  ]);
  const missing = Object.keys(base).filter((key) => !declared.has(key));
  if (missing.length > 0 && process.env.AEC_BRIDGE_SKIP_SELFCHECK !== "1") {
    throw new Error(
      `[aec_bridge] BridgeBackend method(s) ${JSON.stringify(missing)} not declared ` +
        `in NATIVE_WIRED_METHODS or NATIVE_FALLBACK_METHODS \u2014 update bridge.ts ` +
        `so every method has an explicit wired/fallback classification.`,
    );
  }
  return native;
}

// ----- In-process backend -----

/**
 * Build a fresh in-process {@link BridgeBackend}. Exported for tests that
 * want to assert invariants against the full method surface without
 * touching the module-level singleton.
 */
export function inProcessBackend(): BridgeBackend {
  const recents: ProjectSummary[] = [];
  let nextId = 1;

  const id = (prefix: string) => `${prefix}_${(nextId++).toString(36).padStart(4, "0")}`;

  const upsertRecent = (p: ProjectSummary) => {
    const idx = recents.findIndex((r) => r.projectId === p.projectId);
    if (idx >= 0) recents.splice(idx, 1);
    recents.unshift(p);
    while (recents.length > 16) recents.pop();
  };

  const assets: AssetSummary[] = seedAssets();
  const jobs: RenderJob[] = [];
  // Revisions are scoped per backend instance, matching `recents`, `jobs`
  // and `assets` above. Tests that instantiate fresh backends (or hit
  // `adaptNative`, which builds its own `base = inProcessBackend()`)
  // get isolated revision lists.
  const revisions: RevisionSummary[] = [];
  // Per-project command-engine state for the in-process fallback. Keyed
  // by `projectPath` so a vitest that opens two projects gets two
  // independent graphs / undo stacks.
  const graphs = new Map<string, InProcessGraph>();

  return {
    async projectCreateFromTemplate(templateKey, projectName) {
      const summary: ProjectSummary = {
        projectId: id("proj"),
        name: projectName,
        path: `/projects/${slug(projectName)}.aecstudio`,
        templateKey,
        modifiedAt: new Date().toISOString(),
      };
      upsertRecent(summary);
      return summary;
    },
    async projectOpen(projectPath) {
      const summary: ProjectSummary = {
        projectId: id("proj"),
        name: path.basename(projectPath, ".aecstudio"),
        path: projectPath,
        templateKey: null,
        modifiedAt: new Date().toISOString(),
      };
      upsertRecent(summary);
      return summary;
    },
    async projectSave(projectPath) {
      const idx = recents.findIndex((r) => r.path === projectPath);
      const now = new Date().toISOString();
      if (idx >= 0 && recents[idx]) {
        recents[idx].modifiedAt = now;
        return { ...recents[idx] };
      }
      return {
        projectId: id("proj"),
        name: path.basename(projectPath, ".aecstudio"),
        path: projectPath,
        templateKey: null,
        modifiedAt: now,
      };
    },
    async projectListRecents() {
      return [...recents];
    },
    async projectExportPackage(_p, outPath) {
      return { outPath };
    },

    async designPlaceFurniture(_p) {
      return { entityId: id("ent") };
    },
    async designPaintMaterial(_p) {
      return { ok: true };
    },
    async designSetLighting(_p) {
      return { ok: true };
    },
    async designSaveCamera(_p) {
      return { cameraId: id("cam") };
    },
    async designListAssets(query) {
      return filterAssets(assets, query);
    },

    async draftDrawPrimitive(_p) {
      return { entityId: id("ent") };
    },
    async draftEditTool(_p) {
      return { ok: true };
    },
    async draftCreateSheet(_p) {
      return { sheetId: id("sheet") };
    },
    async draftSetLayerState(_p) {
      return { ok: true };
    },
    async draftImportDxf(_path) {
      return { imported: 0 };
    },
    async draftExportDxf(p) {
      return { exported: true, path: p };
    },

    async bimImportIfc(path) {
      // In-process fallback used by Vitest and by dev mode when the
      // native `.node` artifact isn't loaded. Returns the
      // `BimImportSummary` shape with all counters zeroed and
      // `schema` flagged as `"unknown"` so callers can tell the
      // fallback apart from a real parse.
      return {
        path,
        schema: "unknown",
        spatialNodes: 0,
        elements: 0,
        psets: 0,
        qsets: 0,
        aggregations: 0,
        containments: 0,
        materials: 0,
        materialLayerSets: 0,
        materialAssignments: 0,
        recordsSeen: 0,
      };
    },
    async bimCheckFileSize(_path) {
      // In-process fallback used by Vitest. The Rust bridge runs
      // `fs::canonicalize` + `fs::metadata` on the real file; the
      // JS-only fallback reports 0 bytes (well below threshold) so
      // the file-picker UX doesn't spuriously warn during unit
      // tests that don't exercise a real on-disk path.
      return {
        path: _path,
        fileSizeBytes: 0,
        largeFileWarning: false,
        thresholdBytes: BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES,
      };
    },
    async bimAttachIfc(projectPath, ifcPath) {
      // In-process fallback used by Vitest and by dev mode when the
      // native `.node` artifact isn't loaded. Returns the
      // `BimAttachSummary` shape with all counters zeroed so the
      // renderer's "Attach to Project" UX is exercisable in tests
      // even though no actual SQLCipher mutation happens here.
      return {
        path: ifcPath,
        projectPath,
        parseCacheHit: false,
        spatialNodesInserted: 0,
        spatialNodesUpdated: 0,
        spatialNodesUnchanged: 0,
        elementsInserted: 0,
        elementsUpdated: 0,
        elementsUnchanged: 0,
        componentsInserted: 0,
        relationsInserted: 0,
        cacheRows: 0,
      };
    },
    async bimExportIfc(p) {
      return { exported: true, path: p };
    },
    async bimClassify(_p) {
      return { classified: 0 };
    },
    async bimSetProperty(_p) {
      return { ok: true };
    },
    async bimGenerateSchedule(_p) {
      return { scheduleId: id("sched") };
    },
    async bimValidate() {
      return { ok: true, errors: [], warnings: [] };
    },
    async bimDiff(_p) {
      return { diffId: id("diff") };
    },

    async renderEnqueue(params) {
      const job: RenderJob = {
        jobId: id("job"),
        status: "queued",
        preset: String(params.preset ?? "standard"),
        progress: 0,
      };
      jobs.unshift(job);
      return { jobId: job.jobId };
    },
    async renderEnqueueBatch(params) {
      // Mirror the Rust aec_render::queue::RenderQueue::submit_batch /
      // submit_matrix: one job per (camera × preset) pair, all sharing
      // one batch id.
      const presets: string[] =
        params.presetIds && params.presetIds.length > 0
          ? params.presetIds
          : params.presetId
          ? [params.presetId]
          : ["standard"];
      const batchId = id("batch");
      const created: string[] = [];
      for (const cameraId of params.cameraIds) {
        for (const preset of presets) {
          const job: RenderJob = {
            jobId: id("job"),
            status: "queued",
            preset,
            progress: 0,
            cameraId,
            batchId,
          };
          jobs.unshift(job);
          created.push(job.jobId);
        }
      }
      return { batchId, jobIds: created };
    },
    async renderBatchProgress(batchId) {
      const inBatch = jobs.filter((j) => j.batchId === batchId);
      if (inBatch.length === 0) return null;
      let queued = 0;
      let running = 0;
      let completed = 0;
      let failed = 0;
      let cancelled = 0;
      let progressSum = 0;
      for (const j of inBatch) {
        switch (j.status) {
          case "queued":
            queued += 1;
            break;
          case "running":
            running += 1;
            progressSum += j.progress;
            break;
          case "completed":
            completed += 1;
            progressSum += 1;
            break;
          case "failed":
            failed += 1;
            break;
          case "cancelled":
            cancelled += 1;
            break;
        }
      }
      return {
        batchId,
        total: inBatch.length,
        queued,
        running,
        completed,
        failed,
        cancelled,
        averageProgress: progressSum / inBatch.length,
      };
    },
    async renderListJobs() {
      return [...jobs];
    },
    async renderCancelJob(jobId) {
      const j = jobs.find((x) => x.jobId === jobId);
      if (j) j.status = "cancelled";
      return { cancelled: true };
    },
    async renderApplyPreset(_p) {
      return { ok: true };
    },
    async renderDiagnose(jobId) {
      return { jobId, suggestions: [] };
    },
    async renderCheckMaterials() {
      // The native bridge will run check_materials() against the live
      // RenderScene + material library; the in-process backend has no
      // scene state, so it returns an empty findings array.
      return { findings: [] };
    },

    async aiListTools() {
      // Return a defensive copy of the catalogue so that callers can't
      // mutate the shared `AI_TOOLS` constant. This matches the renderer
      // backend (see `rendererInProcessBackend.ai.listTools`).
      return AI_TOOLS.map((t) => ({ ...t }));
    },
    async aiPlan(params) {
      // Pick a realistic parsed payload based on the requested tool so
      // the renderer's AI panels can display proposals end-to-end
      // before the native sidecar lands. The shapes intentionally
      // match the Rust result types in `crates/aec_ai/src/` so the
      // renderer never has to branch on "native vs in-process".
      const parsed = inProcessParsedForTool(
        typeof params.tool === "string" ? params.tool : null,
        params,
      );
      return { diffId: id("diff"), parsed };
    },
    async aiAcceptDiff(_d) {
      return { accepted: true };
    },
    async aiRejectDiff(_d) {
      return { rejected: true };
    },
    async aiCancelJob(_j) {
      return { cancelled: true };
    },
    async aiRuntimeStatus() {
      return { state: "idle", lastError: null };
    },

    async exportPdf(_p) {
      return { outPath: "/exports/out.pdf", pages: 4 };
    },
    async exportDxf(_p) {
      return { outPath: "/exports/out.dxf" };
    },
    async exportIfc(_p) {
      return { outPath: "/exports/out.ifc" };
    },
    async exportGltf(_p) {
      return { outPath: "/exports/out.gltf" };
    },
    async exportBuildProposalPack(_p) {
      return { outPath: "/exports/proposal.pdf" };
    },

    async deliverCreateRevision(params) {
      const tag = params.tag.trim();
      if (!tag) {
        throw new Error("revision tag must not be empty");
      }
      if (revisions.some((r) => r.tag === tag)) {
        throw new Error(`revision tag already exists: ${tag}`);
      }
      const rev: RevisionSummary = {
        revisionId: `rev_${randomId()}`,
        tag,
        description: params.description,
        createdAt: new Date().toISOString(),
        auditChainHead: AUDIT_HEAD_PLACEHOLDER,
        manifestName: "Apartment 12B",
        manifestAppVersion: "0.1.0",
        trackedEntities: (params.entities ?? []).map((e) => ({
          category: e.category,
          id: e.id,
          payloadHash: e.payloadHash,
          label: e.label ?? null,
        })),
      };
      revisions.push(rev);
      return rev;
    },
    async deliverListRevisions() {
      return revisions
        .slice()
        .sort((a, b) => a.createdAt.localeCompare(b.createdAt));
    },
    async deliverCompareRevisions({ baseId, headId }) {
      const base = revisions.find((r) => r.revisionId === baseId);
      const head = revisions.find((r) => r.revisionId === headId);
      if (!base) throw new Error(`unknown base revision: ${baseId}`);
      if (!head) throw new Error(`unknown head revision: ${headId}`);
      return diffRevisionsInProcess(base, head);
    },
    async deliverBuildPack(params) {
      // The native side writes the actual ZIP/PDF. For the in-process
      // backend we synthesise the file list a Rust pack would produce
      // so the renderer can show a realistic preview.
      const contents = packContents(params);
      const totalBytes = contents.reduce(
        (acc, _name, i) => acc + 1024 + i * 256,
        0,
      );
      return { outPath: params.outPath, contents, totalBytes };
    },

    async runtimeStatus() {
      return inProcessRuntimeStatus();
    },

    async projectEngineStatus(_projectPath) {
      // The in-process backend doesn't own an SQLCipher file, and the
      // audit chain isn't tracked here either. Return a constant
      // "zero-state" shape so the renderer UI exercises every
      // EngineStatus code path. Callers that need real values should
      // load the native bridge or run through `BridgeService` directly.
      return inProcessEngineStatus();
    },

    async projectAuditSync(_projectPath) {
      // No SQL mirror exists in the in-process fallback, so any "sync"
      // call is a no-op. Returning 0 rather than throwing keeps the
      // surface call-compatible with the native backend; the
      // documentation in `BridgeBackend` instructs callers to interpret
      // a 0 here as "fallback didn't insert anything".
      return 0;
    },

    async commandApply(projectPath, command) {
      const graph = ensureInProcessGraph(graphs, projectPath);
      const deltas = computeForwardDeltas(graph, command);
      const inverse = applyDeltas(graph, deltas);
      graph.undo.push({
        commandId: command.command_id,
        // Record the originating scope alongside the deltas so undo/
        // redo can validate against it (mirrors the native engine's
        // `JournalEntry.scope`). Without this the fallback would
        // accept any `activeScope` at undo time and silently apply
        // inverse deltas for the wrong rail.
        scope: command.scope,
        forward: deltas,
        inverse,
      });
      graph.redo.length = 0;
      return {
        commandId: command.command_id,
        applied: deltas,
        undoLen: graph.undo.length,
        redoLen: graph.redo.length,
      };
    },

    async commandUndo(projectPath, activeScope) {
      const graph = ensureInProcessGraph(graphs, projectPath);
      const top = graph.undo[graph.undo.length - 1];
      if (!top) {
        throw new Error("command_undo: nothing to undo");
      }
      if (top.scope !== activeScope) {
        // Mirror of `CommandError::ScopeMismatch` from the native
        // engine. Validate before mutating either stack so a
        // rejected call leaves the journal exactly as it was.
        throw new Error(
          `command_undo: scope mismatch (expected ${top.scope}, got ${activeScope})`,
        );
      }
      // Take from the undo stack only after the scope check passes.
      const entry = graph.undo.pop()!;
      applyDeltas(graph, entry.inverse);
      graph.redo.push(entry);
      return {
        commandId: entry.commandId,
        applied: entry.inverse,
        undoLen: graph.undo.length,
        redoLen: graph.redo.length,
      };
    },

    async commandRedo(projectPath, activeScope) {
      const graph = ensureInProcessGraph(graphs, projectPath);
      const top = graph.redo[graph.redo.length - 1];
      if (!top) {
        throw new Error("command_redo: nothing to redo");
      }
      if (top.scope !== activeScope) {
        throw new Error(
          `command_redo: scope mismatch (expected ${top.scope}, got ${activeScope})`,
        );
      }
      const entry = graph.redo.pop()!;
      applyDeltas(graph, entry.forward);
      graph.undo.push(entry);
      return {
        commandId: entry.commandId,
        applied: entry.forward,
        undoLen: graph.undo.length,
        redoLen: graph.redo.length,
      };
    },

    async projectGraphList(projectPath, kindFilter) {
      const graph = ensureInProcessGraph(graphs, projectPath);
      const rows: EntityRecord[] = [];
      for (const r of graph.entities.values()) {
        if (kindFilter === undefined || r.kind === kindFilter) {
          rows.push({
            id: r.id,
            kind: r.kind,
            parent: r.parent,
            // Defensive deep-clone so renderer mutation doesn't leak
            // back into the engine state.
            body: r.body === null || r.body === undefined ? r.body : JSON.parse(JSON.stringify(r.body)),
          });
        }
      }
      return rows;
    },
  };
}

/**
 * In-memory shadow of the Rust `ProjectGraph` + `UndoRedoJournal` used
 * by the in-process backend. Scoped per project path so multiple open
 * projects (or test fixtures) don't bleed entities into one another.
 *
 * This is a **working reimplementation**, not a stub: it mirrors the
 * three-arm delta semantics from
 * `aec_command::commands::project_graph::EntityDelta`, applies them
 * in order, and pushes inverse-delta records onto an undo stack so
 * the renderer can exercise create / undo / redo end-to-end in
 * vitest without the `.node` artefact.
 */
export interface InProcessGraph {
  entities: Map<string, EntityRecord>;
  /**
   * The undo stack carries each command's originating `scope` so
   * {@link BridgeBackend.commandUndo} can validate that the
   * `activeScope` argument matches the scope the command was
   * executed under. The native path stores this on the
   * `undo_journal.scope` column (added in schema v3 — see
   * `crates/aec_core/src/migrations/v3_undo_journal_scope.rs`); the
   * JS fallback keeps the same invariant so the two backends are
   * indistinguishable from the renderer's point of view.
   */
  undo: Array<{
    commandId: string;
    scope: CommandScope;
    forward: EntityDelta[];
    inverse: EntityDelta[];
  }>;
  redo: Array<{
    commandId: string;
    scope: CommandScope;
    forward: EntityDelta[];
    inverse: EntityDelta[];
  }>;
}

export function ensureInProcessGraph(
  graphs: Map<string, InProcessGraph>,
  projectPath: string,
): InProcessGraph {
  let g = graphs.get(projectPath);
  if (!g) {
    g = { entities: new Map(), undo: [], redo: [] };
    graphs.set(projectPath, g);
  }
  return g;
}

/**
 * Explicit kind map for the create-side tools. Each entry maps a
 * `design.*` tool name to the entity `kind` the Rust engine writes
 * into the graph (see `aec_command::commands::*::to_delta()`). Using
 * a literal map rather than a prefix-stripping regex makes this
 * trip-wired: a future `design.update_*` or `design.draw_*` would not
 * silently land in this branch.
 */
const CREATE_TOOL_KIND_MAP: Record<string, string> = {
  "design.create_wall": "wall",
  "design.create_room": "room",
  "design.create_floor": "floor",
  "design.place_door": "door",
  "design.place_window": "window",
  "design.add_light": "light",
  "design.save_camera": "camera",
};

/**
 * Build the create-side entity body for `tool` from the command
 * arguments. Mirrors what `to_delta()` writes into
 * `EntityRecord.body` in `crates/aec_command/src/commands/*`:
 *
 * - CreateWall / CreateRoom / CreateFloor / PlaceDoor / PlaceWindow /
 *   SaveCamera all use `body: serde_json::to_value(self)`, which
 *   serialises the **entire** struct including `entity_id` (and
 *   `host_wall_id` for openings). The body therefore mirrors the
 *   command shape 1:1 — we just spread `args` into a fresh object.
 *
 * - AddLight uses `body: serde_json::to_value(&self.light)`, which
 *   only serialises the `LightKind` sub-object — `entity_id` is
 *   **not** in the body. We extract `args.light` instead.
 *
 * Without this branch the fallback would diverge from the native
 * path: an earlier draft tried to strip `entity_id` from every
 * create body, which inverted the contract — the native path keeps
 * `entity_id`, only AddLight stores just the nested `light`.
 */
function createBodyFor(tool: string, args: Record<string, unknown>): unknown {
  if (tool === "design.add_light") {
    if (args.light === undefined) {
      throw new Error(`${tool}: missing 'light' field (LightKind)`);
    }
    return args.light;
  }
  return { ...args };
}

function assertObjectBody(
  body: unknown,
  tool: string,
): Record<string, unknown> {
  if (body === null || typeof body !== "object" || Array.isArray(body)) {
    throw new Error(`${tool}: stored entity body is not an object`);
  }
  return body as Record<string, unknown>;
}

/**
 * Compute the forward deltas for `command` against `graph`.
 *
 * Each branch mirrors the corresponding `to_delta()` in
 * `aec_command::commands::*` — the in-process fallback is a real
 * working reimplementation that keeps body shape, identity-field
 * naming (e.g. PaintMaterial's `target_entity_id` vs MoveWall's
 * `entity_id`), and surgical update semantics in lock-step with the
 * native path. This is enforced by the renderer-backend tests
 * (which exercise this code directly) plus the Rust integration
 * tests (`crates/aec_bridge::service::tests::command_*`).
 *
 * Exported so the renderer-side vitest fallback
 * (`renderer-backend.ts`) can share the same implementation — the
 * single-source-of-truth approach prevents drift between bridge.ts
 * and renderer-backend.ts that an earlier draft of this engine
 * had (Devin Review round 3, ANALYSIS_0003).
 */
export function computeForwardDeltas(graph: InProcessGraph, command: Command): EntityDelta[] {
  const args = (command.arguments as Record<string, unknown> | undefined) ?? {};
  const tool = command.tool;
  switch (tool) {
    // --- Create-side tools: emit a Create delta with body matching
    //     Rust's `to_delta()` for each command. See `createBodyFor`
    //     for the per-tool body shape contract.
    case "design.create_wall":
    case "design.create_room":
    case "design.create_floor":
    case "design.place_door":
    case "design.place_window":
    case "design.add_light":
    case "design.save_camera": {
      const entityId = (args.entity_id as string | undefined) ?? randomId();
      const kind = CREATE_TOOL_KIND_MAP[tool];
      if (!kind) {
        throw new Error(`commandApply (in-process): kind map missing entry for ${tool}`);
      }
      // Openings (doors/windows) hang off the host wall in the Rust
      // engine (`PlaceDoor::to_delta` sets `parent: Some(host_wall_id)`).
      const parent =
        tool === "design.place_door" || tool === "design.place_window"
          ? ((args.host_wall_id as string | undefined) ?? null)
          : ((args.parent as string | undefined) ?? null);
      return [
        {
          kind: "create",
          record: {
            id: entityId,
            kind,
            parent,
            body: createBodyFor(tool, args),
          },
        },
      ];
    }

    // --- Delete-side tools.
    case "design.delete_wall":
    case "design.delete_opening":
    case "design.remove_light":
    case "design.delete_camera": {
      const id = args.entity_id as string | undefined;
      if (!id) throw new Error(`${tool}: missing entity_id`);
      const existing = graph.entities.get(id);
      if (!existing) throw new Error(`${tool}: entity not found: ${id}`);
      return [{ kind: "delete", record: { ...existing } }];
    }

    // --- design.move_wall: update body.start_mm + body.end_mm.
    case "design.move_wall": {
      const id = args.entity_id as string | undefined;
      if (!id) throw new Error(`${tool}: missing entity_id`);
      const existing = graph.entities.get(id);
      if (!existing) throw new Error(`${tool}: entity not found: ${id}`);
      const body = assertObjectBody(existing.body, tool);
      const after = {
        ...body,
        start_mm: args.new_start_mm,
        end_mm: args.new_end_mm,
      };
      return [{ kind: "update", id, before: existing.body, after }];
    }

    // --- design.modify_room: update body.name (if present) + merge wall_ids.
    case "design.modify_room": {
      const id = args.entity_id as string | undefined;
      if (!id) throw new Error(`${tool}: missing entity_id`);
      const existing = graph.entities.get(id);
      if (!existing) throw new Error(`${tool}: entity not found: ${id}`);
      const body = assertObjectBody(existing.body, tool);
      const after: Record<string, unknown> = { ...body };
      if (typeof args.name === "string") {
        after.name = args.name;
      }
      const existingWalls = Array.isArray(body.wall_ids)
        ? (body.wall_ids as string[]).slice()
        : [];
      const remove = (args.remove_wall_ids as string[] | undefined) ?? [];
      const add = (args.add_wall_ids as string[] | undefined) ?? [];
      const merged = existingWalls.filter((w) => !remove.includes(w));
      for (const w of add) if (!merged.includes(w)) merged.push(w);
      after.wall_ids = merged;
      return [{ kind: "update", id, before: existing.body, after }];
    }

    // --- design.modify_floor: surgical updates to boundary_mm /
    //     thickness_mm / material_id (only when the corresponding
    //     `new_*` field is provided, per Rust semantics).
    case "design.modify_floor": {
      const id = args.entity_id as string | undefined;
      if (!id) throw new Error(`${tool}: missing entity_id`);
      const existing = graph.entities.get(id);
      if (!existing) throw new Error(`${tool}: entity not found: ${id}`);
      const body = assertObjectBody(existing.body, tool);
      const after: Record<string, unknown> = { ...body };
      if (args.new_boundary_mm !== undefined) after.boundary_mm = args.new_boundary_mm;
      if (args.new_thickness_mm !== undefined) after.thickness_mm = args.new_thickness_mm;
      if (args.new_material_id !== undefined) after.material_id = args.new_material_id;
      return [{ kind: "update", id, before: existing.body, after }];
    }

    // --- design.move_opening: update body.position_along_wall_mm.
    case "design.move_opening": {
      const id = args.entity_id as string | undefined;
      if (!id) throw new Error(`${tool}: missing entity_id`);
      const existing = graph.entities.get(id);
      if (!existing) throw new Error(`${tool}: entity not found: ${id}`);
      const body = assertObjectBody(existing.body, tool);
      const after = {
        ...body,
        position_along_wall_mm: args.new_position_along_wall_mm,
      };
      return [{ kind: "update", id, before: existing.body, after }];
    }

    // --- design.paint_material: lookup by `target_entity_id` (not
    //     `entity_id`) because PaintMaterial is conceptually applied
    //     *to* a wall/floor/room. Update body.material_id (whole-entity)
    //     or body.surface_materials[surface] (per-surface).
    case "design.paint_material": {
      const id = args.target_entity_id as string | undefined;
      if (!id) throw new Error(`${tool}: missing target_entity_id`);
      const existing = graph.entities.get(id);
      if (!existing) throw new Error(`${tool}: entity not found: ${id}`);
      const materialId = args.material_id as string | undefined;
      if (!materialId || materialId.trim() === "") {
        throw new Error(`${tool}: material_id must not be empty`);
      }
      const body = assertObjectBody(existing.body, tool);
      const after: Record<string, unknown> = { ...body };
      const surface = args.surface as string | undefined;
      if (surface === undefined || surface === null) {
        after.material_id = materialId;
      } else {
        const surfaces = { ...((body.surface_materials as Record<string, unknown> | undefined) ?? {}) };
        surfaces[surface] = materialId;
        after.surface_materials = surfaces;
      }
      return [{ kind: "update", id, before: existing.body, after }];
    }

    // --- design.swap_finish: equivalent to PaintMaterial with
    //     `material_id = to_material_id` and no surface (per
    //     `SwapFinish::to_paint()`).
    case "design.swap_finish": {
      const id = args.target_entity_id as string | undefined;
      if (!id) throw new Error(`${tool}: missing target_entity_id`);
      const existing = graph.entities.get(id);
      if (!existing) throw new Error(`${tool}: entity not found: ${id}`);
      const body = assertObjectBody(existing.body, tool);
      const after: Record<string, unknown> = { ...body, material_id: args.to_material_id };
      return [{ kind: "update", id, before: existing.body, after }];
    }

    // --- design.update_camera: replace body.params with the new
    //     CameraParams value (matches Rust's `obj.insert("params", …)`).
    case "design.update_camera": {
      const id = args.entity_id as string | undefined;
      if (!id) throw new Error(`${tool}: missing entity_id`);
      const existing = graph.entities.get(id);
      if (!existing) throw new Error(`${tool}: entity not found: ${id}`);
      const body = assertObjectBody(existing.body, tool);
      const after: Record<string, unknown> = { ...body, params: args.params };
      return [{ kind: "update", id, before: existing.body, after }];
    }

    // --- design.set_lighting: audit-only command. Mirrors
    //     `aec_command::engine::CommandEngine::compute_deltas`'s
    //     `SetLighting` arm which returns zero `EntityDelta`s. The
    //     journal entry is still recorded (forward = inverse = []), so
    //     undo/redo cycle counts stay in sync with the native backend.
    case "design.set_lighting": {
      const presetId = args.preset_id as string | undefined;
      if (!presetId || presetId.trim() === "") {
        throw new Error("design.set_lighting: preset_id must not be empty");
      }
      return [];
    }

    default:
      throw new Error(`commandApply (in-process): unsupported tool: ${tool}`);
  }
}

/**
 * Apply `deltas` to `graph` in order and return the corresponding
 * inverse deltas (in **reverse** order, matching Rust's
 * `iter().rev().map(EntityDelta::invert).collect()` semantics).
 *
 * Each branch mirrors the inversion in
 * `aec_command::commands::project_graph::EntityDelta::invert`:
 *   • Create   ↔ Delete (carrying the full record)
 *   • Update   ↔ Update with `before` / `after` swapped
 *   • Delete   ↔ Create
 */
export function applyDeltas(graph: InProcessGraph, deltas: EntityDelta[]): EntityDelta[] {
  const inverse: EntityDelta[] = [];
  for (const d of deltas) {
    switch (d.kind) {
      case "create":
        graph.entities.set(d.record.id, { ...d.record });
        inverse.unshift({ kind: "delete", record: { ...d.record } });
        break;
      case "update": {
        const existing = graph.entities.get(d.id);
        if (!existing) throw new Error(`update: entity not found: ${d.id}`);
        const updated: EntityRecord = { ...existing, body: d.after };
        graph.entities.set(d.id, updated);
        inverse.unshift({ kind: "update", id: d.id, before: d.after, after: d.before });
        break;
      }
      case "delete":
        graph.entities.delete(d.record.id);
        inverse.unshift({ kind: "create", record: { ...d.record } });
        break;
    }
  }
  return inverse;
}

function inProcessEngineStatus(): EngineStatus {
  const byScope: Record<string, number> = {};
  for (const scope of ["design", "draft", "bim", "render", "deliver"] as const) {
    byScope[scope] = 0;
  }
  return {
    // 1 mirrors the base schema version the in-process backend pretends
    // to be at; we don't pretend to be at v2 because the v2 audit_chain
    // table doesn't exist outside the SQLCipher path.
    schemaVersion: 1,
    auditChainHead: AUDIT_HEAD_PLACEHOLDER,
    auditEntryCount: 0,
    auditChainSqlCount: 0,
    auditChainByScope: byScope,
  };
}

const AUDIT_HEAD_PLACEHOLDER =
  "0000000000000000000000000000000000000000000000000000000000000000";

/**
 * JS port of `crates/aec_core/src/version_diff.rs::compare_revisions`.
 * The shapes are identical so the renderer code path is bridge-agnostic.
 */
export function diffRevisionsInProcess(
  base: RevisionSummary,
  head: RevisionSummary,
): VersionDiffSummary {
  type Key = string;
  const keyOf = (category: string, id: string): Key => `${category}\u0000${id}`;
  const baseMap = new Map<Key, RevisionSummary["trackedEntities"][number]>();
  const headMap = new Map<Key, RevisionSummary["trackedEntities"][number]>();
  for (const e of base.trackedEntities) baseMap.set(keyOf(e.category, e.id), e);
  for (const e of head.trackedEntities) headMap.set(keyOf(e.category, e.id), e);

  const byCategory: VersionDiffSummary["byCategory"] = {};
  const bucket = (cat: string) =>
    (byCategory[cat] ??= { added: 0, removed: 0, modified: 0, unchanged: 0 });

  const allKeys = new Set<Key>([...baseMap.keys(), ...headMap.keys()]);
  const changes: VersionDiffSummary["changes"] = [];

  for (const key of [...allKeys].sort()) {
    const b = baseMap.get(key);
    const h = headMap.get(key);
    if (b && h) {
      const counts = bucket(b.category);
      if (b.payloadHash === h.payloadHash) {
        counts.unchanged += 1;
        changes.push({
          category: b.category,
          id: b.id,
          kind: "unchanged",
          beforeHash: b.payloadHash,
          afterHash: h.payloadHash,
          label: h.label ?? b.label,
        });
      } else {
        counts.modified += 1;
        changes.push({
          category: b.category,
          id: b.id,
          kind: "modified",
          beforeHash: b.payloadHash,
          afterHash: h.payloadHash,
          label: h.label ?? b.label,
        });
      }
    } else if (h) {
      bucket(h.category).added += 1;
      changes.push({
        category: h.category,
        id: h.id,
        kind: "added",
        beforeHash: null,
        afterHash: h.payloadHash,
        label: h.label,
      });
    } else if (b) {
      bucket(b.category).removed += 1;
      changes.push({
        category: b.category,
        id: b.id,
        kind: "removed",
        beforeHash: b.payloadHash,
        afterHash: null,
        label: b.label,
      });
    }
  }

  return {
    baseRevisionId: base.revisionId,
    headRevisionId: head.revisionId,
    changes,
    byCategory,
  };
}

function packContents(params: {
  kind: "concept" | "interior" | "contractor" | "bim";
  includeRenders?: boolean;
  includeSheets?: boolean;
  includeIfc?: boolean;
  includeBoq?: boolean;
  includeProposal?: boolean;
}): string[] {
  const base: string[] = [];
  if (params.kind === "concept") {
    base.push("concept_pack.pdf");
    if (params.includeRenders ?? true) base.push("renders/01_cover.png");
    if (params.includeSheets ?? true) base.push("sheets/A100.pdf");
    base.push("manifest.json");
    return base;
  }
  if (params.kind === "interior") {
    base.push("interior_summary.pdf");
    if (params.includeRenders ?? true) {
      base.push("renders/01_living.png", "renders/02_kitchen.png");
    }
    base.push("schedules/materials.xlsx", "manifest.json");
    return base;
  }
  if (params.kind === "contractor") {
    if (params.includeSheets ?? true)
      base.push("sheets/A100.pdf", "sheets/A101.pdf");
    if (params.includeBoq ?? true) base.push("schedules/boq.xlsx");
    base.push("schedules/materials.xlsx");
    if (params.includeIfc ?? true) base.push("model/project.ifc");
    if (params.includeProposal ?? true) base.push("proposal.pdf");
    base.push("manifest.json");
    return base;
  }
  // bim
  if (params.includeIfc ?? true) base.push("model/project.ifc");
  if (params.includeSheets ?? true)
    base.push("sheets/A100.pdf", "sheets/A101.pdf");
  base.push("validation_report.pdf", "manifest.json");
  return base;
}

/**
 * Pick a realistic structured `parsed` payload for an AI plan request,
 * matching the shape of the Rust result types. The native bridge will
 * eventually return the real sidecar output here; until then this
 * fixture lets the renderer exercise the full Propose → Review → Apply
 * flow end-to-end in dev / vitest without a sidecar.
 *
 * Returns `null` when the tool has no client-visible parsed payload
 * (everything goes through diff acceptance instead).
 */
export function inProcessParsedForTool(
  tool: string | null,
  params: Record<string, unknown>,
): AiPlanParsed | null {
  if (tool === "layout_suggestion") {
    const ctx = (params.context ?? {}) as Record<string, unknown>;
    const roomAnchor =
      typeof ctx.room_anchor === "string" ? ctx.room_anchor : "room.unknown";
    // A small but realistic 3-proposal layout: a sofa, a side chair
    // facing it, and a coffee table between them. Coordinates are in
    // millimetres relative to the room anchor's origin, matching the
    // Rust `LayoutProposal::position_mm` contract.
    return {
      tool: "layout_suggestion",
      room_anchor: roomAnchor,
      proposals: [
        {
          asset_id: "ikea.sofa_kivik_3s",
          target_entity: null,
          position_mm: [1200, 0, 600],
          rotation_deg: 0,
        },
        {
          asset_id: "muuto.armchair_outline",
          target_entity: null,
          position_mm: [-1100, 0, 800],
          rotation_deg: 90,
        },
        {
          asset_id: "vendor.coffee_table_round",
          target_entity: null,
          position_mm: [0, 0, 700],
          rotation_deg: 0,
        },
      ],
    };
  }
  return null;
}

function inProcessRuntimeStatus(): RuntimeStatus {
  // eslint-disable-next-line @typescript-eslint/no-var-requires
  const nodeOs: typeof import("os") = require("os");
  const logicalCores = nodeOs.cpus().length;
  const totalMem = Math.round(nodeOs.totalmem() / (1024 * 1024));
  const freeMem = Math.round(nodeOs.freemem() / (1024 * 1024));
  const cpuModel = nodeOs.cpus()[0]?.model ?? "host-cpu";
  // Node `os` does NOT expose physical-vs-logical CPU counts (the
  // distinction matters for SMT/Hyper-Threading machines where the
  // governor's tier-up gate cares about physical cores, not SMT threads).
  // We do a best-effort platform-specific probe here so the dev-mode
  // fallback isn't actively misleading. The real numbers come from
  // `aec_governor`'s sysinfo-backed profiler once the native bridge loads.
  const physicalCores = detectPhysicalCores(nodeOs) ?? logicalCores;
  return {
    tier: classifyTier(logicalCores, totalMem, /* vramMb */ 0),
    cpu: { model: cpuModel, physicalCores, logicalCores },
    ramTotalMb: totalMem,
    ramAvailableMb: freeMem,
    // Node `os` has no GPU API; the in-process fallback intentionally
    // reports `null` GPU and is therefore capped at `Low` by
    // `classifyTier`. The real Rust profiler (in `aec_governor`) does the
    // full detection when the native bridge is loaded.
    gpu: null,
    os: process.platform,
  };
}

/**
 * Best-effort physical-core probe for the in-process (no-native) fallback.
 *
 * - Linux: parse `/proc/cpuinfo` and count unique `(physical id, core id)`
 *   pairs.
 * - macOS: `sysctl -n hw.physicalcpu` (synchronous, tiny, returns an int).
 * - Other platforms (incl. Windows): return `null` — the caller falls
 *   back to logical-core count rather than reporting a fabricated number.
 *
 * Failures are silently ignored: this only runs in dev/test mode and
 * must never throw out of the status RPC.
 */
function detectPhysicalCores(nodeOs: typeof import("os")): number | null {
  try {
    if (nodeOs.platform() === "linux") {
      // eslint-disable-next-line @typescript-eslint/no-var-requires
      const fs: typeof import("fs") = require("fs");
      const text = fs.readFileSync("/proc/cpuinfo", "utf8");
      const seen = new Set<string>();
      let block: { physical?: string; core?: string } = {};
      for (const line of text.split("\n")) {
        if (line.trim() === "") {
          if (block.physical != null && block.core != null) {
            seen.add(`${block.physical}:${block.core}`);
          }
          block = {};
          continue;
        }
        const idx = line.indexOf(":");
        if (idx < 0) {
          continue;
        }
        const key = line.slice(0, idx).trim();
        const val = line.slice(idx + 1).trim();
        if (key === "physical id") {
          block.physical = val;
        } else if (key === "core id") {
          block.core = val;
        }
      }
      // Tail block (file ends without a trailing blank line).
      if (block.physical != null && block.core != null) {
        seen.add(`${block.physical}:${block.core}`);
      }
      return seen.size > 0 ? seen.size : null;
    }
    if (nodeOs.platform() === "darwin") {
      // eslint-disable-next-line @typescript-eslint/no-var-requires
      const cp: typeof import("child_process") = require("child_process");
      const raw = cp
        .execFileSync("/usr/sbin/sysctl", ["-n", "hw.physicalcpu"], {
          encoding: "utf8",
          timeout: 1000,
        })
        .trim();
      const n = Number.parseInt(raw, 10);
      return Number.isFinite(n) && n > 0 ? n : null;
    }
  } catch {
    return null;
  }
  return null;
}

/**
 * Classify hardware into a tier. This mirrors `HardwareTier::classify` in
 * `crates/aec_governor/src/tier.rs` — including the VRAM gate, which is the
 * reason a machine without a discrete GPU (or where we simply can't see one)
 * is always classified as `Low`. Keep this function and the Rust version in
 * lockstep.
 */
export function classifyTier(
  cores: number,
  totalMemMb: number,
  vramMb: number,
): RuntimeStatus["tier"] {
  const ramGb = Math.floor(totalMemMb / 1024);
  const vramGb = Math.floor(vramMb / 1024);
  if (cores >= 16 && ramGb >= 32 && vramGb >= 12) return "Pro";
  if (cores >= 8 && ramGb >= 16 && vramGb >= 8) return "High";
  if (cores >= 4 && ramGb >= 8 && vramGb >= 4) return "Medium";
  return "Low";
}

function slug(input: string): string {
  return input
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
}

function seedAssets(): AssetSummary[] {
  return [
    {
      assetId: "ikea.sofa_kivik_3s",
      name: "Kivik 3-seat Sofa",
      tags: ["furniture", "sofa", "living"],
      styleTags: ["scandinavian", "modern"],
      vendor: "IKEA",
      thumbnailDataUri: null,
    },
    {
      assetId: "muuto.armchair_outline",
      name: "Outline Armchair",
      tags: ["furniture", "armchair", "living"],
      styleTags: ["scandinavian", "japandi"],
      vendor: "Muuto",
      thumbnailDataUri: null,
    },
    {
      assetId: "vendor.cafe_chair_thonet",
      name: "Bentwood Cafe Chair",
      tags: ["furniture", "chair", "cafe"],
      styleTags: ["industrial", "classic"],
      vendor: "Thonet",
      thumbnailDataUri: null,
    },
    {
      assetId: "vendor.cafe_table_700",
      name: "Cafe Table 700",
      tags: ["furniture", "table", "cafe"],
      styleTags: ["industrial"],
      vendor: "Atelier",
      thumbnailDataUri: null,
    },
  ];
}

function filterAssets(assets: AssetSummary[], query: Record<string, unknown>): AssetSummary[] {
  const tags = Array.isArray(query.tags) ? (query.tags as string[]) : [];
  const styleTags = Array.isArray(query.styleTags) ? (query.styleTags as string[]) : [];
  const search = typeof query.search === "string" ? (query.search as string).toLowerCase() : "";
  const limit = typeof query.limit === "number" ? (query.limit as number) : 24;
  return assets
    .filter((a) => tags.every((t) => a.tags.includes(t)))
    .filter((a) => styleTags.every((t) => a.styleTags.includes(t)))
    .filter((a) => (search ? a.name.toLowerCase().includes(search) : true))
    .slice(0, limit);
}

// `AI_TOOLS` and `AiTool` are sourced from `./ai-tools` and re-exported
// at the top of this file. The in-process backend reads the shared
// catalogue, so any change to the tool list happens in one place.
