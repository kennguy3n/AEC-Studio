/**
 * Typed renderer-side helpers around the command engine
 * (`aec_command::engine::CommandEngine`) — bridges the renderer's
 * UI actions to the Rust engine via the
 * `command:apply` / `command:undo` / `command:redo` /
 * `project:graphList` IPC channels exposed by
 * `apps/desktop/electron/preload.ts`.
 *
 * Each helper builds a fully-formed `Command` envelope (matching
 * `aec_command::commands::Command`'s on-wire serde shape — including
 * `command_id` / `ts` / `scope` / `actor` / `tool` / `arguments`)
 * so callers don't have to hand-roll the envelope or invent ids.
 *
 * The engine carries scope on every command, so `undo` / `redo`
 * take the currently active scope as well — a guard against
 * cross-scope undos (e.g. you can't undo a `design.create_wall`
 * while in the bim workflow).
 */

import { aec } from "./aec";

/**
 * On-wire `Command` envelope. Mirrors
 * `aec_command::commands::Command` 1:1, including its serde
 * `rename_all = "snake_case"` derives.
 */
export interface Command {
  command_id: string;
  ts: string;
  scope: CommandScope;
  actor: CommandActor;
  tool: string;
  arguments: unknown;
}

export type CommandScope = "design" | "draft" | "bim" | "render" | "deliver";

export type CommandActor =
  | { kind: "user" }
  | { kind: "ai"; tool: string };

/**
 * Three-arm tagged union mirroring
 * `aec_command::commands::project_graph::EntityDelta`.
 */
export type EntityDelta =
  | { kind: "create"; record: EntityRecord }
  | { kind: "update"; id: string; before: unknown; after: unknown }
  | { kind: "delete"; record: EntityRecord };

export interface EntityRecord {
  id: string;
  kind: string;
  parent: string | null;
  body: unknown;
}

export interface CommandApplyResult {
  commandId: string;
  applied: EntityDelta[];
  undoLen: number;
  redoLen: number;
}

/**
 * Mint a fresh command id. Mirrors
 * `aec_core::types::CommandId::new` — a `cmd_` prefix plus a 22-char
 * URL-safe random suffix. We do not require strict format
 * compatibility with the Rust mint because the engine treats the
 * id as opaque (validated only as a non-empty string).
 */
export function newCommandId(): string {
  const rand = Array.from(crypto.getRandomValues(new Uint8Array(16)))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  return `cmd_${rand}`;
}

/**
 * Build a fully-formed `Command` envelope for a single tool
 * invocation. Callers pass the `tool` tag and the `arguments`
 * payload; this helper attaches a fresh `command_id`, a current
 * `ts`, the workflow `scope`, and a `user` actor.
 */
export function buildUserCommand(
  scope: CommandScope,
  tool: string,
  args: Record<string, unknown>,
): Command {
  return {
    command_id: newCommandId(),
    ts: new Date().toISOString(),
    scope,
    actor: { kind: "user" },
    tool,
    arguments: args,
  };
}

/**
 * Build a fully-formed `Command` envelope for an AI tool
 * invocation. The `aiTool` parameter is recorded in the audit
 * envelope so reviewers can trace each AI-issued change back to
 * its originating tool.
 */
export function buildAiCommand(
  scope: CommandScope,
  aiTool: string,
  tool: string,
  args: Record<string, unknown>,
): Command {
  return {
    command_id: newCommandId(),
    ts: new Date().toISOString(),
    scope,
    actor: { kind: "ai", tool: aiTool },
    tool,
    arguments: args,
  };
}

/**
 * Apply a command. Returns the engine's response (commitId +
 * resulting `EntityDelta[]` + post-call undo/redo stack depths).
 */
export async function commandApply(
  projectPath: string,
  command: Command,
): Promise<CommandApplyResult> {
  return (await aec.command.apply(projectPath, command)) as unknown as CommandApplyResult;
}

/**
 * Undo the most recently applied command in `activeScope`.
 */
export async function commandUndo(
  projectPath: string,
  activeScope: CommandScope,
): Promise<CommandApplyResult> {
  return (await aec.command.undo(projectPath, activeScope)) as unknown as CommandApplyResult;
}

/**
 * Redo the most recently undone command in `activeScope`.
 */
export async function commandRedo(
  projectPath: string,
  activeScope: CommandScope,
): Promise<CommandApplyResult> {
  return (await aec.command.redo(projectPath, activeScope)) as unknown as CommandApplyResult;
}

/**
 * List the project graph. Pass `kindFilter` (e.g. `"wall"`,
 * `"camera"`) to narrow; pass `undefined` for everything.
 */
export async function projectGraphList(
  projectPath: string,
  kindFilter?: string,
): Promise<EntityRecord[]> {
  return (await aec.command.listGraph(projectPath, kindFilter)) as EntityRecord[];
}

// ----- Typed design helpers -----

/**
 * Build + apply a `design.create_wall` command. Coordinates are
 * 2-D mm vectors in the project's working plane. The result's
 * `applied[0]` is the `Create` delta carrying the wall's new id.
 */
export async function createWall(
  projectPath: string,
  args: {
    entity_id?: string;
    start_mm: [number, number];
    end_mm: [number, number];
    height_mm: number;
    thickness_mm: number;
    material_id?: string | null;
  },
): Promise<CommandApplyResult> {
  return commandApply(
    projectPath,
    buildUserCommand("design", "design.create_wall", args),
  );
}

/** Build + apply `design.delete_wall`. */
export async function deleteWall(
  projectPath: string,
  entityId: string,
): Promise<CommandApplyResult> {
  return commandApply(
    projectPath,
    buildUserCommand("design", "design.delete_wall", { entity_id: entityId }),
  );
}

/** Build + apply `design.paint_material`. */
export async function paintMaterial(
  projectPath: string,
  args: { entity_id: string; material_id: string },
): Promise<CommandApplyResult> {
  return commandApply(
    projectPath,
    buildUserCommand("design", "design.paint_material", args),
  );
}

/** Build + apply `design.save_camera`. */
export async function saveCamera(
  projectPath: string,
  args: {
    entity_id?: string;
    name: string;
    position_mm: [number, number, number];
    target_mm: [number, number, number];
    fov_deg: number;
  },
): Promise<CommandApplyResult> {
  return commandApply(
    projectPath,
    buildUserCommand("design", "design.save_camera", args),
  );
}

/**
 * Build + apply `design.set_lighting`. The Rust
 * `aec_command::commands::lighting::SetLighting` struct is
 * `{ preset_id: String }` — there is no `entity_id` because the command
 * is audit-only and applies a scene-wide lighting preset rather than
 * mutating a specific entity. Field name must be `preset_id` (not
 * `preset`) to roundtrip through serde on the native backend.
 */
export async function setLighting(
  projectPath: string,
  args: { preset_id: string },
): Promise<CommandApplyResult> {
  return commandApply(
    projectPath,
    buildUserCommand("design", "design.set_lighting", args),
  );
}
