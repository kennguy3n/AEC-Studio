/**
 * Renderer-side fallback backend used by Vitest. Mirrors the
 * in-process backend that the Electron main process uses when the
 * native bridge artefact is absent.
 *
 * # Factory pattern (not a singleton)
 *
 * {@link rendererInProcessBackend} is a *factory*, not a singleton.
 * Every call creates a fresh closure with isolated `recents`,
 * `current`, `activeListeners`, and `nextId` state.
 *
 * Two consumers, two intentionally different lifetimes:
 *
 * 1. **Production renderer**: `apps/desktop/renderer/src/api/aec.ts`
 *    calls the factory exactly *once* at module-evaluation time and
 *    exports the result as the `aec` constant. Because ES modules
 *    are singletons within a renderer process, every component import
 *    of `aec` gets the same backend instance — so opening a project
 *    in one mode page is visible from every other mode page, the
 *    StatusBar polling site, etc. This matches main-process semantics
 *    where the active-project state lives in one
 *    `apps/desktop/electron/active-project.ts` module.
 *
 * 2. **Vitest fixtures**: `renderer-backend-project-lifecycle.test.ts`
 *    and similar tests call the factory *per test* to get a hermetic
 *    backend instance. This is what makes the test suite deterministic
 *    against an in-process backend: project A opened in test 1 cannot
 *    leak into test 2's `recents` list, listener subscriptions cleared
 *    in `afterEach` cannot accidentally fire against a later test's
 *    state, and the `nextId` counter starts at 1 for every test
 *    (stable assertions on generated thread / camera / job ids).
 *
 * Earlier iterations briefly returned a top-level singleton and let
 * tests reach into module-private state for reset; that pattern
 * caused intermittent test-ordering flakes whenever a previous test
 * left a `defaultThreadId` poller running. The factory contract
 * eliminates that class of flake entirely — there is no shared state
 * to reset because there is no shared state.
 */

import type { AecApi } from "../../../electron/preload";
import { AI_TOOLS } from "../../../electron/ai-tools";
import {
  BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES,
  isBuiltInPresetId,
  applyDeltas,
  classifyTier,
  computeForwardDeltas,
  diffRevisionsInProcess,
  ensureInProcessGraph,
  inProcessParsedForTool,
  packContents,
  type Command,
  type CommandScope,
  type EntityRecord,
  type InProcessGraph,
  type RevisionSummary,
  type VersionDiffSummary,
} from "../../../electron/bridge";

interface Recent {
  projectId: string;
  name: string;
  path: string;
  templateKey: string | null;
  modifiedAt: string;
}

export function rendererInProcessBackend(): AecApi {
  // The renderer in-process backend models two distinct pieces of
  // state: the *recents history* (which persists across close+reopen,
  // mirroring the user's most-recently-opened project list) and the
  // *currently-open project* (which becomes `null` after `close()`,
  // mirroring the main-process `clearActiveProjectPath()` contract).
  //
  // An earlier version conflated these by returning `recents[0]` from
  // `current()` — which caused `close()` to be a no-op (since clearing
  // the head would also destroy the user's recents list). Splitting
  // them lets `close()` clear the active slot without wiping recents,
  // matching production's main-process semantics exactly.
  const recents: Recent[] = [];
  let current: Recent | null = null;

  // Phase 17 Group B Task 12. In-memory thumbnail blobs keyed by
  // project path. The native bridge persists these into the project's
  // SQLCipher DB; this fallback only needs to round-trip through the
  // same renderer code path so a vitest spec exercising `ProjectCard`
  // sees a non-null `<img>` after `setThumbnail` resolves. Validation
  // mirrors `crates/aec_bridge/src/service.rs::project_set_thumbnail`
  // verbatim so error contracts cannot drift.
  type ThumbnailRow = {
    png: Uint8Array;
    width: number;
    height: number;
    updatedAt: string;
  };
  const thumbnails = new Map<string, ThumbnailRow>();
  const THUMBNAIL_MAX_BYTES = 1024 * 1024;
  const THUMBNAIL_PNG_MAGIC = new Uint8Array([
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
  ]);
  const isValidPngHeader = (b: Uint8Array): boolean => {
    if (b.length < THUMBNAIL_PNG_MAGIC.length) return false;
    for (let i = 0; i < THUMBNAIL_PNG_MAGIC.length; i += 1) {
      if (b[i] !== THUMBNAIL_PNG_MAGIC[i]) return false;
    }
    return true;
  };
  // Listeners subscribed via `project.onActiveProjectChange`. The
  // in-process backend mirrors the main-process `active-project.ts`
  // notification contract: every transition (open / create / save of
  // the active project / close) fires `notifyActiveChange()` with
  // the latest summary (or `null` for close). This lets the renderer
  // hook's push-subscription useEffect exercise the same code path
  // under vitest that production exercises against the real Electron
  // IPC channel.
  //
  // Production-timing parity: in Electron, `webContents.send(
  // "project:active-changed", summary)` from the main process is
  // delivered to the renderer via a Chromium IPC dispatch on a
  // macrotask boundary. The corresponding `ipcRenderer.invoke`
  // response that resolves the awaiter's promise is delivered on a
  // SEPARATE macrotask boundary; the renderer's microtask drain
  // (await continuations) runs BETWEEN the two. As a result, the
  // push listener in production fires AFTER
  // `useActiveProject.openProject`'s continuation has already called
  // `updateProject(summary)` and committed `projectPathRef`, so the
  // listener's path-equality guard (see useActiveProject.tsx:370)
  // correctly skips the redundant write and there is no double
  // commit.
  //
  // To match this exactly under vitest, we defer the listener
  // iteration to a macrotask (`setTimeout(..., 0)`). Without the
  // defer, the listeners would fire synchronously inside `upsert()`
  // — BEFORE the awaiter's continuation runs — and the path-equality
  // guard would observe a stale `projectPathRef`, causing a spurious
  // second commit that production never sees. Tests that need to
  // assert on listener side-effects await one macrotask tick (e.g.
  // `await new Promise((r) => setTimeout(r, 0))`) before reading the
  // observed events, mirroring the way a real renderer would let the
  // IPC channel drain.
  //
  // The summary snapshot is captured *eagerly* (at call time, not at
  // listener-invocation time) so the listener sees the state that
  // was active when the transition happened. This matches
  // production where the main process serialises the summary into
  // the IPC payload at send time, not at receive time. Listener
  // membership IS re-read at invocation time (via `Array.from`), so
  // a late `unsubscribe()` that lands between queue and run still
  // drops the callback — matching the production semantics of
  // `ipcRenderer.off` removing the listener before the queued IPC
  // event reaches it.
  const activeListeners = new Set<(s: Recent | null) => void>();
  const notifyActiveChange = () => {
    const snapshot = current === null ? null : { ...current };
    setTimeout(() => {
      for (const l of Array.from(activeListeners)) {
        try {
          l(snapshot);
        } catch {
          // Match the main-process tracker's defensive try/catch — a
          // listener throwing must not corrupt other listeners or
          // the backend state. Production has no global handler
          // that would catch a sync throw out of
          // `ipcRenderer.on(...)`.
        }
      }
    }, 0);
  };
  let nextId = 1;
  const newId = (prefix: string) =>
    `${prefix}_${(nextId++).toString(36).padStart(4, "0")}`;
  const upsert = (r: Recent) => {
    const i = recents.findIndex((x) => x.projectId === r.projectId);
    if (i >= 0) recents.splice(i, 1);
    recents.unshift(r);
    while (recents.length > 16) recents.pop();
    current = { ...r };
    notifyActiveChange();
  };

  const assets = [
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

  // Phase 17 Group B Task 11. Mirror of the bridge's
  // `MaterialLibrary::with_default_pack()` seed — 8 starter
  // materials whose albedos / style tags / per-id roughness +
  // metallic overrides match the Rust default pack exactly
  // (`crates/aec_materials/src/library.rs`). Defaults
  // (metallic 0, roughness 0.6, ior 1.45, emissive [0,0,0],
  // transmission 0) come from the Rust `PbrMaterial::new`
  // constructor — keep these aligned with the napi backend's
  // values so renderer tests against the in-process backend
  // exercise the same id space the napi backend serves in
  // production.
  const materials: {
    materialId: string;
    name: string;
    albedo: [number, number, number];
    metallic: number;
    roughness: number;
    ior: number;
    transmission: number;
    emissive: [number, number, number];
    styleTags: string[];
    tags: string[];
  }[] = [
    {
      materialId: "mat:oak_light",
      name: "Light Oak",
      albedo: [0.78, 0.66, 0.5],
      metallic: 0,
      roughness: 0.6,
      ior: 1.45,
      transmission: 0,
      emissive: [0, 0, 0],
      styleTags: ["scandinavian", "warm"],
      tags: [],
    },
    {
      materialId: "mat:walnut",
      name: "Walnut",
      albedo: [0.34, 0.21, 0.14],
      metallic: 0,
      roughness: 0.6,
      ior: 1.45,
      transmission: 0,
      emissive: [0, 0, 0],
      styleTags: ["industrial", "warm"],
      tags: [],
    },
    {
      materialId: "mat:concrete_polished",
      name: "Polished Concrete",
      albedo: [0.55, 0.55, 0.56],
      metallic: 0,
      roughness: 0.45,
      ior: 1.45,
      transmission: 0,
      emissive: [0, 0, 0],
      styleTags: ["industrial", "minimal"],
      tags: [],
    },
    {
      materialId: "mat:linen_oat",
      name: "Oat Linen",
      albedo: [0.85, 0.78, 0.66],
      metallic: 0,
      roughness: 0.85,
      ior: 1.45,
      transmission: 0,
      emissive: [0, 0, 0],
      styleTags: ["japandi", "warm"],
      tags: [],
    },
    {
      materialId: "mat:matte_white",
      name: "Matte White Paint",
      albedo: [0.92, 0.92, 0.91],
      metallic: 0,
      roughness: 0.6,
      ior: 1.45,
      transmission: 0,
      emissive: [0, 0, 0],
      styleTags: ["minimal"],
      tags: [],
    },
    {
      materialId: "mat:terracotta",
      name: "Terracotta Tile",
      albedo: [0.78, 0.42, 0.32],
      metallic: 0,
      roughness: 0.6,
      ior: 1.45,
      transmission: 0,
      emissive: [0, 0, 0],
      styleTags: ["mediterranean", "warm"],
      tags: [],
    },
    {
      materialId: "mat:brushed_brass",
      name: "Brushed Brass",
      albedo: [0.78, 0.68, 0.42],
      metallic: 0.9,
      roughness: 0.3,
      ior: 1.45,
      transmission: 0,
      emissive: [0, 0, 0],
      styleTags: ["art_deco", "warm"],
      tags: [],
    },
    {
      materialId: "mat:marble_carrara",
      name: "Carrara Marble",
      albedo: [0.92, 0.92, 0.93],
      metallic: 0,
      roughness: 0.45,
      ior: 1.45,
      transmission: 0,
      emissive: [0, 0, 0],
      styleTags: ["classical", "minimal"],
      tags: [],
    },
  ];

  return {
    project: {
      createFromTemplate: async (templateKey, projectName) => {
        const summary: Recent = {
          projectId: newId("proj"),
          name: projectName,
          path: `/projects/${projectName.toLowerCase().replace(/\W+/g, "_")}.aecstudio`,
          templateKey,
          modifiedAt: new Date().toISOString(),
        };
        upsert(summary);
        return summary;
      },
      open: async (projectPath) => {
        const summary: Recent = {
          projectId: newId("proj"),
          name:
            projectPath.split("/").pop()?.replace(".aecstudio", "") ??
            "Project",
          path: projectPath,
          templateKey: null,
          modifiedAt: new Date().toISOString(),
        };
        upsert(summary);
        return summary;
      },
      save: async (projectPath) => {
        const idx = recents.findIndex((r) => r.path === projectPath);
        const now = new Date().toISOString();
        if (idx >= 0 && recents[idx]) {
          recents[idx].modifiedAt = now;
          // Keep the active-project mirror in lockstep with the
          // recents entry so the next `current()` call reflects the
          // refreshed `modifiedAt` — matching production where the
          // main-process active-project tracker is updated on save.
          // Only fire `notifyActiveChange()` when the saved project
          // IS the active one; saving a non-active project from a
          // future background-export pipeline must not promote that
          // project into the active slot (same contract as
          // `setActiveProjectIfMatchesActive` on the main side).
          if (current?.path === projectPath) {
            current = { ...recents[idx] };
            notifyActiveChange();
          }
          return { ...recents[idx] };
        }
        return {
          projectId: newId("proj"),
          name:
            projectPath.split("/").pop()?.replace(".aecstudio", "") ??
            "Project",
          path: projectPath,
          templateKey: null,
          modifiedAt: now,
        };
      },
      setThumbnail: async (projectPath, png, width, height) => {
        // Phase 17 Group B Task 12. The in-process renderer fallback
        // mirrors the bridge's validation pipeline so vitest specs
        // see the same error surface as production. We keep the
        // accepted blob in `thumbnails` so a subsequent `getThumbnail`
        // returns it (otherwise a UI test that exercises the full
        // capture→display loop would silently regress to the gradient
        // placeholder).
        if (!png || png.length === 0) {
          throw new Error("project_set_thumbnail: PNG buffer is empty");
        }
        if (!isValidPngHeader(png)) {
          throw new Error(
            "project_set_thumbnail: buffer does not start with PNG magic header",
          );
        }
        if (width === 0 || height === 0 || width > 4096 || height > 4096) {
          throw new Error(
            `project_set_thumbnail: dimensions out of range (1..=4096); got ${width}\u00D7${height}`,
          );
        }
        if (png.length > THUMBNAIL_MAX_BYTES) {
          throw new Error(
            `project_set_thumbnail: PNG buffer is ${png.length} bytes; max is ${THUMBNAIL_MAX_BYTES} bytes`,
          );
        }
        thumbnails.set(projectPath, {
          png: new Uint8Array(png),
          width,
          height,
          updatedAt: new Date().toISOString(),
        });
        return { ok: true as const };
      },
      getThumbnail: async (projectPath) => {
        const t = thumbnails.get(projectPath);
        if (!t) return null;
        return {
          png: new Uint8Array(t.png),
          width: t.width,
          height: t.height,
          updatedAt: t.updatedAt,
        };
      },
      listRecents: async () => [...recents],
      exportPackage: async (_p, outPath) => ({ outPath }),
      current: async () => ({
        summary: current === null ? null : { ...current },
      }),
      close: async () => {
        // Clear the active-project mirror so `current()` returns `null`
        // after close — matching the main-process `clearActiveProjectPath()`
        // contract. The `recents` history is preserved so a subsequent
        // open from the Home screen's recently-opened list still works.
        current = null;
        notifyActiveChange();
        return { ok: true as const };
      },
      // Push-subscription channel mirroring
      // `aec.project.onActiveProjectChange` from the preload bridge.
      // The renderer's `useActiveProject` hook subscribes once on
      // mount and unsubscribes on unmount; the returned unsubscribe
      // function removes the listener from the internal Set so
      // long-running tests that mount/unmount the provider don't leak.
      onActiveProjectChange: (
        listener: (
          summary: {
            projectId: string;
            name: string;
            path: string;
            templateKey: string | null;
            modifiedAt: string;
          } | null,
        ) => void,
      ): (() => void) => {
        activeListeners.add(listener);
        return () => {
          activeListeners.delete(listener);
        };
      },
    },
    dialog: {
      openFile: async () => ({ canceled: true, paths: [] }),
      openDirectory: async () => ({ canceled: true, path: null }),
      saveFile: async () => ({ canceled: true, path: null }),
    },
    design: {
      placeFurniture: async () => ({ entityId: newId("ent") }),
      paintMaterial: async () => ({ ok: true }),
      setLighting: async () => ({ ok: true }),
      saveCamera: async () => ({ cameraId: newId("cam") }),
      listAssets: async (query) => {
        const q = query as {
          tags?: string[];
          styleTags?: string[];
          search?: string;
          limit?: number;
        };
        return assets
          .filter((a) => (q.tags ?? []).every((t) => a.tags.includes(t)))
          .filter((a) =>
            (q.styleTags ?? []).every((t) => a.styleTags.includes(t)),
          )
          .filter((a) =>
            q.search
              ? a.name.toLowerCase().includes(q.search.toLowerCase())
              : true,
          )
          .slice(0, q.limit ?? 24);
      },
      // Phase 17 Group B Task 11. Renderer-side in-process
      // fallback mirror of the main-process `seedMaterials()` —
      // 8 starter materials whose ids / albedos / style tags
      // match the bridge's `MaterialLibrary::with_default_pack()`
      // exactly. The fallback exists so the `MaterialPanel`
      // renders deterministically in unit tests without a
      // running napi backend, and so the renderer can be
      // exercised under `vite preview` (no Electron host) for
      // visual regression work.
      listMaterials: async (query) => {
        const q = query as {
          tags?: string[];
          styleTags?: string[];
          search?: string;
          limit?: number;
        };
        return materials
          .filter((m) => (q.tags ?? []).every((t) => m.tags.includes(t)))
          .filter((m) =>
            (q.styleTags ?? []).every((t) => m.styleTags.includes(t)),
          )
          .filter((m) =>
            q.search
              ? m.name.toLowerCase().includes(q.search.toLowerCase())
              : true,
          )
          .slice(0, q.limit ?? materials.length);
      },
      // Range-validation mirrors `validateMaterialUpdate` in
      // `apps/desktop/electron/bridge.ts` so the renderer
      // surface sees the same `invalid: …` error string the
      // napi backend would emit.
      updateMaterial: async (materialId, update) => {
        const idx = materials.findIndex((m) => m.materialId === materialId);
        if (idx < 0) {
          throw new Error(`invalid: material \`${materialId}\` not found`);
        }
        const inUnit = (v: number | undefined, label: string) => {
          if (v !== undefined && !(v >= 0 && v <= 1)) {
            throw new Error(`invalid: ${label} must be in [0.0, 1.0]`);
          }
        };
        inUnit(update.metallic, "metallic");
        inUnit(update.roughness, "roughness");
        inUnit(update.transmission, "transmission");
        if (
          update.ior !== undefined &&
          !(update.ior >= 1 && update.ior <= 5)
        ) {
          throw new Error("invalid: ior must be in [1.0, 5.0]");
        }
        for (const [label, v] of [
          ["albedo", update.albedo],
          ["emissive", update.emissive],
        ] as const) {
          if (v) {
            for (let i = 0; i < 3; i++) {
              if (!(v[i] >= 0 && v[i] <= 1)) {
                throw new Error(
                  `invalid: ${label}[${i}] must be in [0.0, 1.0]`,
                );
              }
            }
          }
        }
        const cur = materials[idx];
        const next = {
          ...cur,
          albedo: update.albedo ?? cur.albedo,
          metallic: update.metallic ?? cur.metallic,
          roughness: update.roughness ?? cur.roughness,
          ior: update.ior ?? cur.ior,
          transmission: update.transmission ?? cur.transmission,
          emissive: update.emissive ?? cur.emissive,
        };
        materials[idx] = next;
        return next;
      },
    },
    draft: {
      drawPrimitive: async () => ({ entityId: newId("ent") }),
      editTool: async () => ({ ok: true }),
      createSheet: async () => ({ sheetId: newId("sheet") }),
      setLayerState: async () => ({ ok: true }),
      importDxf: async () => ({ imported: 0 }),
      // The renderer-side `aec.draft.exportDxf` now takes the full
      // bridge param object `{ dxfPath, projectPath? }` (matching the
      // preload signature). Echo the supplied `dxfPath` so renderer
      // tests can assert on the round-trip.
      exportDxf: async (p) => ({ exported: true, path: p.dxfPath }),
    },
    bim: {
      importIfc: async (path) => ({
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
      }),
      checkFileSize: async (path) => ({
        path,
        fileSizeBytes: 0,
        largeFileWarning: false,
        thresholdBytes: BIM_IMPORT_LARGE_FILE_THRESHOLD_BYTES,
      }),
      attachIfc: async (projectPath, ifcPath) => ({
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
      }),
      // PR-T read-only BIM ops. These mirror the electron-side
      // in-process backend in `electron/bridge.ts` 1:1 (same field
      // names, same zero-finding payloads) so vitest sees the
      // exact same shape the real bridge would produce in dev
      // mode. Both the electron-side stubs (`bridge.ts:1272-1299`)
      // and these renderer-side mocks intentionally do NOT touch
      // the filesystem — they return zeroed-out, wire-format-
      // compliant payloads so the renderer can exercise its
      // status panes against synthetic paths that don't exist
      // on disk. The actual IFC pipeline
      // is exercised end-to-end by
      // `crates/aec_bridge/tests/bim_readonly_ops.rs`.
      exportIfc: async (params) => ({
        sourcePath: params.sourcePath,
        outPath: params.outPath,
        schema: "IFC4",
        bytesWritten: 0,
        parseCacheHit: false,
      }),
      // Mirrors `BimClassifyResult` in `electron/bridge.ts` and the
      // typed preload signature. The in-process fallback can't run
      // the real `bim_classify` (no SQLCipher project on disk under
      // vitest), so it returns the empty-success shape and lets the
      // caller's "0 classified" toast render. Earlier this method
      // returned only `{ classified: 0 }`, which silently hid a
      // production bug where the Bim page sent the wrong params —
      // matching the full shape here means future drift will trip
      // the bridge's `adaptNative` self-check instead of the user.
      classify: async () => ({
        scheme: "ifc",
        classified: 0,
        unchanged: 0,
        skipped: 0,
        details: [],
      }),
      setProperty: async () => ({ ok: true }),
      readScheduleRows: async () => ({ header: [], rows: [] }),
      generateSchedule: async (params) => ({
        scheduleId: newId("sched"),
        kind: params.kind,
        sourcePath: params.sourcePath,
        outPath: params.outPath,
        rows: 0,
        columns: 0,
        bytesWritten: 0,
        parseCacheHit: false,
      }),
      validate: async (params) => ({
        ok: true,
        sourcePath: params.sourcePath,
        schema: "IFC4",
        errors: [],
        warnings: [],
        infos: [],
        parseCacheHit: false,
      }),
      diff: async (params) => ({
        diffId: newId("diff"),
        beforePath: params.beforePath,
        afterPath: params.afterPath,
        beforeSchema: "IFC4",
        afterSchema: "IFC4",
        added: [],
        removed: [],
        modified: [],
        beforeCacheHit: false,
        afterCacheHit: false,
      }),
    },
    render: {
      enqueueRender: async () => ({ jobId: newId("job") }),
      listJobs: async () => [],
      cancelJob: async () => ({ cancelled: true }),
      applyPreset: async (params) => {
        // Validate against the shared `BUILT_IN_PRESET_IDS` constant
        // in `apps/desktop/electron/bridge.ts` — see its doc comment
        // for the dev/production parity rationale.
        const presetId = typeof params.preset === "string" ? params.preset : "";
        if (!isBuiltInPresetId(presetId)) {
          throw new Error(`unknown render preset id \`${presetId}\``);
        }
        return { ok: true };
      },
      diagnose: async (jobId) => ({ jobId, suggestions: [] }),
      enqueueBatch: async (params) => {
        // Mirror the in-process electron backend and the native side:
        // empty cameras/presets *and* unknown preset ids are hard
        // errors, not a silent `["standard"]` fallback. Renderer
        // vitest tests must see the same rejection contract as
        // production. `isBuiltInPresetId` is the single TS-side
        // source of truth — see `BUILT_IN_PRESET_IDS` in
        // `apps/desktop/electron/bridge.ts`.
        const presets =
          params.presetIds && params.presetIds.length > 0
            ? params.presetIds
            : params.presetId
              ? [params.presetId]
              : [];
        if (params.cameraIds.length === 0) {
          throw new Error("renderEnqueueBatch requires at least one camera id");
        }
        if (presets.length === 0) {
          throw new Error("renderEnqueueBatch requires at least one preset id");
        }
        for (const preset of presets) {
          if (!isBuiltInPresetId(preset)) {
            throw new Error(`unknown render preset id \`${preset}\``);
          }
        }
        const batchId = newId("batch");
        const jobIds: string[] = [];
        for (let i = 0; i < params.cameraIds.length * presets.length; i++) {
          jobIds.push(newId("job"));
        }
        return { batchId, jobIds };
      },
      batchProgress: async (batchId) => ({
        batchId,
        total: 0,
        queued: 0,
        running: 0,
        completed: 0,
        failed: 0,
        cancelled: 0,
        averageProgress: 0,
      }),
      checkMaterials: async () => ({ findings: [] }),
      // Phase 17 — output read-back + SSIM compare. The renderer
      // fallback has no rendered output to surface, so each call
      // throws a clear error. Tests that exercise the compare /
      // preview flow are expected to vi.spyOn(aec.render, ...) and
      // return a fixture.
      getOutputImage: async (params: { jobId: string }) => {
        throw new Error(
          `renderGetOutputImage: in-process backend has no output for '${params.jobId}'; mock the bridge call in tests`,
        );
      },
      compareSsim: async (params: { aJobId: string; bJobId: string }) => {
        throw new Error(
          `renderCompareSsim: in-process backend cannot compare ('${params.aJobId}', '${params.bJobId}'); mock the bridge call in tests`,
        );
      },
      setEnvironmentMap: async () => ({ ok: true as const }),
    },
    ai: {
      // Return a defensive copy so callers (and Vitest harnesses) can't
      // mutate the shared catalogue.
      listTools: async () => AI_TOOLS.map((t) => ({ ...t })),
      plan: async (params: Record<string, unknown>) => {
        const tool = typeof params.tool === "string" ? params.tool : null;
        const parsed = inProcessParsedForTool(tool, params);
        return { diffId: newId("diff"), parsed };
      },
      acceptDiff: async (diffId: string) => ({
        accepted: true as const,
        diffId,
        opCount: 0,
        appliedCount: 0,
        skipped: [] as Array<{ opIndex: number; reason: string }>,
        commandIds: [] as string[],
        auditChainHead: "",
      }),
      rejectDiff: async (diffId: string, reason?: string | null) => ({
        rejected: true as const,
        diffId,
        opCount: 0,
        reason: reason ?? null,
        auditChainHead: "",
      }),
      cancelJob: async () => ({ cancelled: true }),
      runtimeStatus: async () => ({ state: "idle", lastError: null }),
      modelAvailability: async () => ({
        tiers: [
          {
            tier: "small" as const,
            name: "Ternary-Bonsai 1.7B (1.58-bit GGUF Q2_0)",
            filename: "Ternary-Bonsai-1.7B-Q2_0.gguf",
            sizeBytes: 463_290_464,
            available: false,
            sizeOnDisk: 0,
          },
          {
            tier: "medium" as const,
            name: "Ternary-Bonsai 4B (1.58-bit GGUF Q2_0)",
            filename: "Ternary-Bonsai-4B-Q2_0.gguf",
            sizeBytes: 1_074_969_344,
            available: false,
            sizeOnDisk: 0,
          },
          {
            tier: "large" as const,
            name: "Ternary-Bonsai 8B (1.58-bit GGUF Q2_0)",
            filename: "Ternary-Bonsai-8B-Q2_0.gguf",
            sizeBytes: 2_182_184_672,
            available: false,
            sizeOnDisk: 0,
          },
        ],
        activeTier: "small" as const,
        modelsDir: "",
      }),
      downloadModel: async (tier: "small" | "medium" | "large") => ({
        tier,
        path: "",
        sizeBytes: 0,
      }),
      downloadProgress: async () => null,
      setActiveTier: async (_tier: "small" | "medium" | "large") => {},
    },
    extensions: {
      // Vitest fallback: there are no extensions loaded in the
      // renderer in-process backend, so by construction there
      // cannot be any load failures to surface. The empty-array
      // contract is the same signal the Settings page uses on the
      // real backend to hide the diagnostics card entirely.
      listLoadDiagnostics: async () => [],
    },
    export: {
      // Vitest fallback for the export IPC namespace. Mirrors the
      // shape AND the strictness of the in-process backend in
      // `apps/desktop/electron/bridge.ts` (which itself mirrors the
      // native napi struct contract): `outPath` and `projectName` are
      // mandatory non-empty strings, missing fields throw the same
      // error shape (`{method}: missing required string field
      // '{field}'`) the electron-side fallback produces. Page count
      // (`exportPdf`) is pinned at `2` to match
      // `aec_export::write_project_pdf` (1 cover + 1 overview); the
      // electron-side in-process fallback uses the same value.
      exportPdf: async (params) => {
        const outPath = requireStringField(params, "exportPdf", "outPath");
        requireStringField(params, "exportPdf", "projectName");
        return { outPath, pages: 2 };
      },
      exportDxf: async (params) => {
        const outPath = requireStringField(params, "exportDxf", "outPath");
        requireStringField(params, "exportDxf", "projectName");
        return { outPath };
      },
      exportIfc: async (params) => {
        const outPath = requireStringField(params, "exportIfc", "outPath");
        requireStringField(params, "exportIfc", "projectName");
        return { outPath };
      },
      exportGltf: async (params) => {
        const outPath = requireStringField(params, "exportGltf", "outPath");
        requireStringField(params, "exportGltf", "projectName");
        return { outPath };
      },
      // Uses the underlying `BridgeBackend.exportBuildProposalPack`
      // method name (NOT the renderer-surface `buildProposalPack`) so
      // the error string matches the electron-side fallback and the
      // native napi `ExportProposalPackParamsJs` struct. This honours
      // the JSDoc contract below: "The error message format is
      // identical across the three layers so renderer tests can
      // assert on the exception text without branching on which
      // backend produced it." Pinned by `export-in-process.test.ts`
      // line 247 against the same regex the electron-side test uses.
      buildProposalPack: async (params) => {
        const outPath = requireStringField(
          params,
          "exportBuildProposalPack",
          "outPath",
        );
        requireStringField(params, "exportBuildProposalPack", "projectName");
        return { outPath };
      },
    },
    deliver: deliverMock(newId),
    command: commandMock(),
    runtime: {
      // Derive the tier from the same `classifyTier` the production
      // backend uses so the test fixture cannot drift away from real
      // classifier behaviour. The fixture deliberately includes a small
      // discrete GPU so the Medium-tier code path is exercised end-to-end.
      status: async () => ({
        tier: classifyTier(
          /* cores */ 4,
          /* totalMemMb */ 16384,
          /* vramMb */ 4096,
        ),
        cpu: { model: "test-cpu", physicalCores: 4, logicalCores: 8 },
        ramTotalMb: 16384,
        ramAvailableMb: 8192,
        gpu: { vendor: "test-gpu", model: "test-mid", vramMb: 4096 },
        os: "test",
      }),
    },
    kchat: kchatMock(newId),
    viewport: viewportMock(),
  };
}

/**
 * Renderer-fallback KChat backend. There's no KChat Desktop instance
 * in vitest, so `status()` always reports the in-memory publisher,
 * `publish()` returns a deterministic `messageId` (the `newId()`
 * counter is shared with every other renderer-mock id source so
 * snapshot tests get stable output), and `ingestReviews()` returns
 * an empty result set.
 */
function kchatMock(newId: (prefix: string) => string) {
  return {
    status: async () =>
      ({
        state: "disconnected",
        publisherKind: "in_memory",
        instanceJson: null,
        // Vitest contexts never open a real project, so the
        // per-project `KChatConfig::default_thread_id` is always
        // absent. The Deliver page's review panel falls back to
        // its `kchat-default` constant when this is null.
        defaultThreadId: null,
        // Vitest mock mirrors `KChatConfig::default()` so component
        // tests that render the Settings card observe a non-
        // disabled bridge by default.
        enabled: true,
      }) as Awaited<ReturnType<AecApi["kchat"]["status"]>>,
    reload: async () =>
      ({
        state: "disconnected",
        publisherKind: "in_memory",
        instanceJson: null,
        defaultThreadId: null,
        enabled: true,
      }) as Awaited<ReturnType<AecApi["kchat"]["reload"]>>,
    setEnabled: async ({ enabled }: { enabled: boolean }) =>
      ({
        state: "disconnected",
        publisherKind: "in_memory",
        instanceJson: null,
        defaultThreadId: null,
        enabled,
      }) as Awaited<ReturnType<AecApi["kchat"]["setEnabled"]>>,
    publish: async (_params: { cardJson: string }) => ({
      messageId: newId("kchat_msg"),
      threadId: "kchat-default",
      publishedAt: new Date("2026-01-01T00:00:00Z").toISOString(),
    }),
    ingestReviews: async (params: {
      threadId: string;
      sinceIso?: string | null;
    }) => ({
      threadId: params.threadId,
      commentsJson: "[]",
      cardsJson: "[]",
    }),
    // Phase 15: vitest contexts never have a real KChat Desktop
    // running, so deeplink open requests + inbound deeplinks are
    // no-ops. We deliberately return a rate_limited-looking
    // shape rather than `{ ok: true }` so renderer code that
    // surfaces "opened" feedback doesn't trigger a misleading
    // toast in headless tests.
    openInDesktop: async (_params: { url: string }) => ({
      ok: false as const,
      reason: "scheme_not_allowed" as const,
    }),
    onDeeplink: (_cb: (route: never) => void) => () => undefined,
  } satisfies AecApi["kchat"];
}

/**
 * Renderer-fallback viewport backend (Phase 12). Vitest doesn't
 * have a GPU adapter, so we return `state: "unavailable"` from
 * `status()`, store the requested width/height in a closure
 * variable so subsequent `resize()` reports the latest values, and
 * keep a tiny in-memory camera so `input({kind:"orbit"})` followed
 * by `requestFrame()` reflects the new pose.
 */
function viewportMock() {
  const camera = {
    position: [5000, 3000, 5000] as [number, number, number],
    target: [0, 0, 0] as [number, number, number],
    up: [0, 1, 0] as [number, number, number],
    fov_y_radians: Math.PI / 3,
  };
  let width = 0;
  let height = 0;
  let frameIndex = 0;
  const cameraJson = () => JSON.stringify(camera);
  return {
    status: async () => ({
      state: "unavailable" as const,
      width,
      height,
      frameIndex,
      gpuDescriptorJson: null,
    }),
    resize: async (params: { width: number; height: number }) => {
      width = params.width;
      height = params.height;
      return {
        state: "unavailable" as const,
        width,
        height,
        frameIndex,
        gpuDescriptorJson: null,
      };
    },
    input: async (params: {
      kind: "orbit" | "pan" | "zoom" | "reset";
      dx?: number;
      dy?: number;
      delta?: number;
    }) => {
      // Trivial position update so tests can observe an effect.
      if (params.kind === "orbit") {
        camera.position[0] += params.dx ?? 0;
        camera.position[1] += params.dy ?? 0;
      } else if (params.kind === "pan") {
        camera.position[0] -= params.dx ?? 0;
        camera.position[1] += params.dy ?? 0;
        camera.target[0] -= params.dx ?? 0;
        camera.target[1] += params.dy ?? 0;
      } else if (params.kind === "zoom") {
        const f = 1 - (params.delta ?? 0) * 0.001;
        camera.position[0] *= f;
        camera.position[1] *= f;
        camera.position[2] *= f;
      } else if (params.kind === "reset") {
        camera.position = [5000, 3000, 5000];
      }
      return { cameraJson: cameraJson() };
    },
    requestFrame: async () => {
      frameIndex += 1;
      return {
        frameIndex,
        width,
        height,
        state: "unavailable" as const,
        cameraJson: cameraJson(),
      };
    },
    // Phase 17 Task 21 — the renderer-fallback backend has no GPU
    // and no Rust viewport service, so it returns null (= "no
    // frame buffer available"). Vitest tests that need a synthetic
    // frame buffer mock the AecApi directly instead.
    readFrameBuffer: async () => null,
  } satisfies AecApi["viewport"];
}

/**
 * Read a **required** `string` field from a renderer-supplied
 * `Record<string, unknown>` params object. Throws when the field is
 * absent / `null` / `undefined` / not a string.
 *
 * Mirrors the electron-side `requireStringField` in
 * `apps/desktop/electron/bridge.ts` so the vitest fallback enforces
 * the same mandatory-field contract as both the in-process backend
 * and the native napi structs in
 * `crates/aec_bridge/src/napi_api.rs`. The error message format
 * (`{method}: missing required string field '{field}'`) is identical
 * across the three layers so renderer tests can assert on the
 * exception text without branching on which backend produced it.
 */
function requireStringField(
  params: Record<string, unknown>,
  method: string,
  key: string,
): string {
  const v = params[key];
  if (typeof v !== "string") {
    throw new Error(`${method}: missing required string field '${key}'`);
  }
  return v;
}

/**
 * Renderer-side fixture for the `deliver` IPC namespace. Mirrors the
 * shape of `apps/desktop/electron/preload.ts` `deliver` so vitest tests
 * can exercise revision + pack composer flows without spinning up the
 * Electron host. The diff helper reuses `diffRevisionsInProcess` from
 * the bridge module so the fixture cannot drift away from production
 * comparison semantics.
 */
function deliverMock(newId: (prefix: string) => string) {
  const revisions: RevisionSummary[] = [];

  return {
    async createRevision(params: {
      tag: string;
      description: string;
      entities?: Array<{
        category: string;
        id: string;
        payloadHash: string;
        label?: string | null;
      }>;
    }): Promise<RevisionSummary> {
      const tag = params.tag.trim();
      if (!tag) throw new Error("revision tag must not be empty");
      if (revisions.some((r) => r.tag === tag)) {
        throw new Error(`revision tag already exists: ${tag}`);
      }
      const rev: RevisionSummary = {
        revisionId: newId("rev"),
        tag,
        description: params.description,
        createdAt: new Date().toISOString(),
        auditChainHead:
          "0000000000000000000000000000000000000000000000000000000000000000",
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
    async listRevisions(): Promise<RevisionSummary[]> {
      return revisions
        .slice()
        .sort((a, b) => a.createdAt.localeCompare(b.createdAt));
    },
    async compareRevisions(params: {
      baseId: string;
      headId: string;
    }): Promise<VersionDiffSummary> {
      const base = revisions.find((r) => r.revisionId === params.baseId);
      const head = revisions.find((r) => r.revisionId === params.headId);
      if (!base) throw new Error(`unknown base revision: ${params.baseId}`);
      if (!head) throw new Error(`unknown head revision: ${params.headId}`);
      return diffRevisionsInProcess(base, head);
    },
    async buildPack(params: {
      kind: "concept" | "interior" | "contractor" | "bim";
      outPath: string;
      // Optional `projectPath` and `projectName` mirror the preload
      // signature so vitest-side callers and electron-side callers
      // share the same typed contract. The vitest fixture doesn't
      // need either value to compute its synthetic inventory, but
      // accepting them keeps the renderer's `aec.deliver.buildPack`
      // call site polymorphic across both backends and prevents
      // structural-typing leakage at the IPC boundary.
      projectPath?: string;
      projectName?: string;
      includeRenders?: boolean;
      includeSheets?: boolean;
      includeIfc?: boolean;
      includeBoq?: boolean;
      includeProposal?: boolean;
      region?: "eu" | "na" | "apac";
    }): Promise<{ outPath: string; contents: string[]; totalBytes: number }> {
      // Reuse the same `packContents` builder the electron in-process
      // backend uses (and which itself pins the inventory shape to the
      // native Rust `aec_export::write_deliver_pack`). Without this,
      // the renderer's vitest fixture returned a simpler 2–3-file
      // inventory while production produced the full per-kind list —
      // flagged in PR-S round 2 "deliverMock in renderer-backend.ts
      // uses simplified inventory that diverges from packContents".
      // Same `?? true` defaults flow through `packContents`, so
      // omitted `include_*` flags match the native + electron
      // behaviour automatically. `totalBytes` is a synthetic function
      // of the file count because the vitest fallback has no real
      // bytes to measure — production replaces this whole call with
      // `deliver_build_pack`'s real ZIP assembly.
      //
      // The formula `acc + 1024 + i * 256` is byte-identical to the
      // electron in-process backend at `apps/desktop/electron/
      // bridge.ts::inProcessBackend()::deliverBuildPack`. The shared
      // shape lets a renderer regression test that covers both
      // surfaces compare against the same synthetic number without
      // branching on which backend is running.
      const contents = packContents(params);
      return {
        outPath: params.outPath,
        contents,
        totalBytes: contents.reduce((acc, _name, i) => acc + 1024 + i * 256, 0),
      };
    },
  };
}

/**
 * Renderer-side fixture for the `command` IPC namespace.
 *
 * The renderer constructs `Command` envelopes; the IPC layer passes
 * them through unchanged. The fixture's responsibility is to model a
 * working create / undo / redo loop the way the Rust engine does —
 * applying forward deltas, capturing inverse deltas, popping on undo,
 * pushing onto redo, and dropping the redo stack on any new apply.
 *
 * Both the per-tool delta computation (`computeForwardDeltas`) and
 * the inverse-delta application loop (`applyDeltas`) are imported
 * from `bridge.ts` so the renderer-side vitest fallback and the
 * electron-side in-process fallback share a *single* implementation.
 * Earlier drafts had three near-identical copies (Rust engine +
 * bridge.ts + renderer-backend.ts) — see Devin Review round 3
 * ANALYSIS_0003. Sharing the TypeScript implementation eliminates
 * the JS-side drift; the Rust ↔ JS parity is enforced by the
 * `crates/aec_bridge::service::tests::command_*` integration tests
 * which run both engines side-by-side.
 */
function commandMock() {
  const graphs = new Map<string, InProcessGraph>();

  return {
    async apply(projectPath: string, command: unknown) {
      const c = command as Command;
      const graph = ensureInProcessGraph(graphs, projectPath);
      const fwd = computeForwardDeltas(graph, c);
      const inv = applyDeltas(graph, fwd);
      graph.undo.push({
        commandId: c.command_id,
        // Tag the entry with the originating command's scope so the
        // mirror of `CommandError::ScopeMismatch` in `undo` / `redo`
        // can reject a stale `activeScope` before either stack moves.
        scope: c.scope,
        forward: fwd,
        inverse: inv,
      });
      graph.redo.length = 0;
      return {
        commandId: c.command_id,
        applied: fwd,
        undoLen: graph.undo.length,
        redoLen: graph.redo.length,
      };
    },
    async undo(projectPath: string, activeScope: string) {
      const graph = ensureInProcessGraph(graphs, projectPath);
      const top = graph.undo[graph.undo.length - 1];
      if (!top) throw new Error("command_undo: nothing to undo");
      if (top.scope !== (activeScope as CommandScope)) {
        throw new Error(
          `command_undo: scope mismatch (expected ${top.scope}, got ${activeScope})`,
        );
      }
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
    async redo(projectPath: string, activeScope: string) {
      const graph = ensureInProcessGraph(graphs, projectPath);
      const top = graph.redo[graph.redo.length - 1];
      if (!top) throw new Error("command_redo: nothing to redo");
      if (top.scope !== (activeScope as CommandScope)) {
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
    async listGraph(
      projectPath: string,
      kindFilter?: string,
    ): Promise<EntityRecord[]> {
      const graph = ensureInProcessGraph(graphs, projectPath);
      const rows: EntityRecord[] = [];
      for (const r of graph.entities.values()) {
        if (kindFilter === undefined || r.kind === kindFilter) {
          rows.push({
            id: r.id,
            kind: r.kind,
            parent: r.parent,
            body:
              r.body === null || r.body === undefined
                ? r.body
                : JSON.parse(JSON.stringify(r.body)),
          });
        }
      }
      return rows;
    },
  };
}
