# AEC Studio — Architecture

---

## High-level architecture

```mermaid
flowchart TB
    subgraph "Electron Renderer (React / TypeScript)"
        Home["Home / project dashboard"]
        ModeRail["Workflow mode rail"]
        Inspector["Inspector panels"]
        AiPanel["AI panel (diff preview)"]
        AssetBrowser["Asset browser"]
        ExportUI["Export / delivery UI"]
    end

    subgraph "Electron Main Process"
        IPC["Secure typed IPC"]
        WinMgr["Window / menu / tray"]
        FilePicker["OS file picker"]
        NativeLoad["N-API addon loader"]
        WorkerSup["Worker supervision"]
    end

    subgraph "Rust Native Core"
        ProjectGraph["Project graph"]
        CmdEngine["Command engine"]
        UndoJournal["Undo / redo journal"]
        GeoIndex["Geometry index"]
        AssetDB["Asset DB"]
        RenderQueue["Render queue"]
        Governor["Resource governor"]
        ExtPerm["Extension permissions"]
        PackageMgr["File package manager"]
        NAPI["N-API bridge"]
    end

    subgraph "Native Render Engine (Rust/wgpu)"
        NativePreview["PBR preview (wgpu)"]
        NativePathTrace["Path tracer (wgpu compute + CPU)"]
        NativeWalkthrough["Walkthrough / panorama"]
    end

    subgraph "CAD Worker (Rust)"
        CADcanvas["wgpu CAD canvas"]
        DXF["DXF read / write"]
        DWG["DWG read / write (native, R12 → R2018)"]
    end

    subgraph "Native BIM Engine (Rust)"
        IfcReader["Native STEP parser (aec_bim::ifc)"]
        IfcWriter["Native STEP writer"]
        Tessellator["Geometry tessellator"]
        BIMCache["BIM cache"]
    end

    subgraph "Local AI Worker"
        LlamaSidecar["llama.cpp / PrismML / MLX"]
        VisionModel["Plan-detection vision model"]
        Embeddings["Embeddings"]
        ToolPlanner["Tool planner (grammar-constrained)"]
    end

    Home --> IPC
    ModeRail --> IPC
    Inspector --> IPC
    AiPanel --> IPC
    AssetBrowser --> IPC
    ExportUI --> IPC

    IPC --> NAPI
    WinMgr --> NAPI
    FilePicker --> NAPI
    NativeLoad --> NAPI
    WorkerSup --> NativePreview
    WorkerSup --> NativePathTrace
    WorkerSup --> CADcanvas
    WorkerSup --> IfcReader
    WorkerSup --> LlamaSidecar

    NAPI --> ProjectGraph
    NAPI --> CmdEngine
    NAPI --> UndoJournal
    NAPI --> GeoIndex
    NAPI --> AssetDB
    NAPI --> RenderQueue
    NAPI --> Governor
    NAPI --> ExtPerm
    NAPI --> PackageMgr

    CmdEngine --> UndoJournal
    CmdEngine --> ProjectGraph
    CmdEngine --> GeoIndex
    RenderQueue --> NativePathTrace
    RenderQueue --> NativeWalkthrough
    GeoIndex --> CADcanvas
    GeoIndex --> IfcReader
    ToolPlanner --> CmdEngine

    NativePathTrace --> NativePreview

    IfcReader --> IfcWriter
    IfcReader --> Tessellator
    IfcReader --> BIMCache

    LlamaSidecar --> VisionModel
    LlamaSidecar --> Embeddings
    LlamaSidecar --> ToolPlanner
```

---

## Recommended stack

| Layer | Technology | Reason |
|---|---|---|
| Desktop shell | Electron | Cross-platform desktop with mature native bridges |
| UI framework | React + TypeScript | Productivity UI, typed IPC, ecosystem |
| Core engine | Rust | Memory safety, performance, geometry indexing, command engine |
| Native viewport | wgpu | Cross-platform GPU (Vulkan / Metal / D3D12 / OpenGL) for 3D + 2D CAD |
| Local database | SQLite / SQLCipher | Local-first, encrypted, single-file project storage |
| Search / hybrid retrieval | SQLite FTS5 + embeddings | Asset search, BIM property search without external services |
| Model runtime | llama.cpp / PrismML sidecar | Local GGUF inference with broad acceleration coverage |
| Apple Silicon | MLX | macOS ARM inference acceleration |
| Render engine | Native Rust path tracer (wgpu compute) + PBR rasterizer | Photoreal final + fast preview, in-process with no external runtime |
| BIM / IFC | Native Rust STEP parser + writer + tessellator (`aec_bim::ifc`) | In-process IFC4 (and IFC2x3 / IFC4x3 on input) with verbatim Pset round-trip; no external dependency |
| Electron bridge | N-API (napi-rs) | Low-overhead Rust ↔ Node.js calls |
| Packaging | electron-builder | Platform installers for macOS and Windows |

---

## Why Rust

Rust is the primary systems language for AEC Studio's core engine. It handles:

- The project graph (spatial hierarchy, geometry, materials, schedules)
- The command engine with an undo/redo journal
- Geometry indexing (BVH, spatial queries, mesh cache)
- The render queue and native path tracer orchestration
- The asset database with content hashing (BLAKE3) and dedup
- Encrypted local storage (SQLite + SQLCipher)
- The resource governor and hardware profiler
- The export engine (PDF, DXF, IFC, glTF, proposal pack)
- The extension permissions enforcer
- The audit trail
- The N-API bridge to Electron and the cross-platform N-API addon

Rust-first matches the substrate patterns from [kennguy3n/knowledge](https://github.com/kennguy3n/knowledge) and [kennguy3n/Tessera](https://github.com/kennguy3n/Tessera), keeping the engine fast, predictable, and free of unsafe code (`unsafe_code = "forbid"` at the workspace level).

---

## Why wgpu

The viewport, 2D CAD canvas, and selection/overlay layers all run on **wgpu**:

- **Cross-platform** — Vulkan on Linux/Windows, Metal on macOS/iOS, D3D12 on Windows, OpenGL fallback.
- **Predictable** — explicit pipelines, no driver-specific surprises.
- **Shared across modes** — 3D Design and 2D CAD reuse the same renderer with different camera/projection setups.

### Viewport split

| Surface | wgpu role | Notes |
|---|---|---|
| 3D Design viewport | Forward + clustered renderer for design preview | Snap overlays, selection halos, gizmos |
| 2D CAD canvas | Orthographic projection, batched line/polyline/hatch | Pixel-perfect snapping, infinite zoom |
| Selection overlay | Stencil + outline pass | Shared across 3D and 2D |
| Hover / measure tools | Lightweight overlay pass | Read-only of the geometry index |
| Final render preview | Texture display | Just shows the native path-tracer / PBR-rasterizer output |

---

## Electron boundary

AEC Studio enforces a **strict security boundary** between the renderer and native capabilities.

```
React renderer → typed IPC → Electron main → N-API → Rust core → in-process engines (path tracer, IFC, CAD) + AI sidecar
```

### Anti-patterns to avoid

| Anti-pattern | Why it's dangerous |
|---|---|
| Renderer directly accessing files | Bypasses OS permission model |
| Renderer launching worker processes | Exposes process control to the web context |
| Renderer holding asset packs in memory | Memory bloat and exposes proprietary asset bytes |
| Renderer parsing IFC | Pulls a heavy native dependency into the renderer; security risk |
| Renderer talking to the AI sidecar directly | Bypasses the safety validator and audit trail |
| Renderer accessing the encrypted DB | Exposes the encryption key to the web context |

### TypeScript API interfaces

```typescript
interface ProjectApi {
  createFromTemplate(templateId: string, options?: ProjectCreateOptions): Promise<ProjectId>;
  open(packagePath: string): Promise<ProjectSummary>;
  save(projectId: ProjectId): Promise<void>;
  listRecents(): Promise<ProjectSummary[]>;
  exportPackage(projectId: ProjectId, target: ExportTarget): Promise<ExportResult>;
}

interface DesignApi {
  placeFurniture(req: PlaceFurnitureRequest): Promise<CommandResult>;
  paintMaterial(req: PaintMaterialRequest): Promise<CommandResult>;
  setLighting(req: SetLightingRequest): Promise<CommandResult>;
  saveCamera(req: SaveCameraRequest): Promise<CameraId>;
  listAssets(query: AssetQuery): Promise<AssetSummary[]>;
}

interface DraftApi {
  drawPrimitive(req: DrawPrimitiveRequest): Promise<CommandResult>;
  editTool(req: EditToolRequest): Promise<CommandResult>;
  createSheet(req: SheetCreateRequest): Promise<SheetId>;
  setLayerState(req: LayerStateRequest): Promise<CommandResult>;
  importDxf(filePath: string): Promise<ImportResult>;
  exportDxf(req: DxfExportRequest): Promise<ExportResult>;
}

interface BimApi {
  importIfc(filePath: string): Promise<ImportResult>;
  exportIfc(req: IfcExportRequest): Promise<ExportResult>;
  classify(req: ClassifyRequest): Promise<CommandResult>;
  setProperty(req: SetPropertyRequest): Promise<CommandResult>;
  generateSchedule(req: ScheduleRequest): Promise<ScheduleResult>;
  validate(req: ValidateRequest): Promise<ValidationReport>;
  diff(packageA: string, packageB: string): Promise<DiffReport>;
}

interface RenderApi {
  enqueueRender(req: RenderRequest): Promise<JobId>;
  listJobs(): Promise<RenderJobStatus[]>;
  cancelJob(jobId: JobId): Promise<void>;
  applyPreset(req: ApplyPresetRequest): Promise<CommandResult>;
  diagnose(req: DiagnoseRequest): Promise<DoctorReport>;
}

interface AiApi {
  listTools(scope: ModeScope): Promise<ToolSchema[]>;
  plan(req: PlanRequest): AsyncIterable<PlanChunk>;
  acceptDiff(diffId: string): Promise<CommandResult>;
  rejectDiff(diffId: string): Promise<void>;
  cancelJob(jobId: string): Promise<void>;
  runtimeStatus(): Promise<RuntimeStatus>;
}

interface ExportApi {
  exportPdf(req: PdfExportRequest): Promise<ExportResult>;
  exportDxf(req: DxfExportRequest): Promise<ExportResult>;
  exportIfc(req: IfcExportRequest): Promise<ExportResult>;
  exportGltf(req: GltfExportRequest): Promise<ExportResult>;
  buildProposalPack(req: ProposalPackRequest): Promise<ExportResult>;
}
```

---

## 9.1 Rust command engine

Every state mutation in AEC Studio is a **command**: a typed, serializable record applied through the command engine. AI actions reuse the same engine via the diff preview.

### Command structure

```json
{
  "command_id": "cmd_01HG5...",
  "ts": "2026-05-19T17:14:21Z",
  "scope": "design",
  "actor": { "kind": "user" },
  "tool": "design.place_furniture",
  "arguments": {
    "asset_id": "asset_sofa_modern_3seat_v2",
    "anchor": { "type": "room", "id": "space_living_room" },
    "offset_mm": { "x": 0, "y": 0, "z": 0 },
    "rotation_deg": 0,
    "scale": 1.0
  },
  "diff": {
    "diff_id": "diff_a1b2c3",
    "applied": [
      { "kind": "create", "entity_id": "furn_01HG5...", "entity_kind": "furniture_instance" }
    ]
  },
  "audit": {
    "hash": "blake3:...",
    "previous_hash": "blake3:...",
    "signed": false
  }
}
```

### Capabilities

- Each command declares its scope (`design`, `draft`, `bim`, `render`, `deliver`).
- Each command declares a bounded set of entities it can mutate.
- The undo/redo journal stores reversible deltas, not snapshots, for compact history.
- AI-emitted commands always carry `actor.kind = "ai"` and `actor.tool` for provenance.
- Commands are append-only in the audit log; the project graph is materialized by replaying commands or loading a checkpoint.

---

## 9.2 Native render engine

```
crates/aec_render/
├── bvh.rs                      # SAH BVH2 builder, two-level instancing
├── intersect.rs                # Möller–Trumbore + stack-based BVH traversal
├── path_trace.rs               # CPU megakernel path tracer
├── gpu_trace.rs                # wgpu compute path tracer (fallback to CPU)
├── shaders/path_trace.wgsl     # WGSL compute kernel
├── material.rs                 # Principled BSDF (Lambert + GGX, Schlick Fresnel, Smith)
├── light_sampling.rs           # Sun / area / point / sky / IES with MIS
├── denoise.rs                  # Edge-aware bilateral / NLM denoiser
├── scheduler.rs                # Tile scheduler, adaptive sampling, cancellation
├── preview.rs                  # Native PBR rasterized preview pipeline
├── final_render.rs             # Final-render pipeline (PNG / EXR output)
├── walkthrough.rs              # Camera-path animation + optional ffmpeg MP4 stitch
├── panorama.rs                 # Equirectangular 360° panorama pipeline
├── scene.rs                    # RenderScene / RenderCamera / RenderLight
├── preset.rs                  # Quick / Standard / High / Studio / Preview / Walkthrough / Panorama
├── queue.rs                    # Render queue (single, batch, matrix), resume on failure
└── doctor.rs                   # Render diagnostics

crates/aec_viewport/
├── pbr_preview.rs              # PBR forward rasterizer (wgpu) used by preview.rs
├── shaders/pbr.wgsl            # PBR fragment shader (metallic/roughness + IBL)
├── shaders/sky.wgsl            # Hosek–Wilkie procedural sky
└── sky.rs                      # Sky parameters → GPU uniform binding
```

### Pipelines

| Pipeline | Trigger | Engine | Output |
|---|---|---|---|
| Preview | Viewport refresh / camera change | PBR rasterizer (wgpu) | RGB texture |
| Final | User-initiated render | Path tracer (wgpu compute, CPU fallback) | PNG / EXR + denoise |
| Batch | Render queue | Path tracer | Multi-image pack |
| Walkthrough | User-initiated | Path tracer + camera path | MP4 / image sequence |
| Panorama | User-initiated | Path tracer (equirectangular camera) | EXR / JPG |

**Key decision:** Rendering is **fully in-process**. There is no Blender (or any other external renderer) involvement at runtime — not as a linked library, not as a subprocess, not as an embedded interpreter. Phase 9 PRs #9–#12 implemented a complete native CPU + GPU path tracer (informed by reading [kennguy3n/cycles](https://github.com/kennguy3n/cycles) as a reference) and a native PBR rasterizer for preview. Eliminating the external worker simplifies packaging (no Blender install required), removes the GPL boundary, improves crash isolation (Rust memory safety vs. Python subprocess), and removes IPC latency from the preview hot path.

---

## 9.3 2D CAD worker

```
crates/aec_cad/
├── primitives/                 # Line, polyline, arc, circle, ellipse, spline, hatch, text
├── editing/                    # Move, copy, rotate, scale, mirror, offset, trim, extend
├── precision/                  # Snaps, grid, ortho, polar, tracking, constraints
├── layers/                     # Layer state, freeze/thaw, lineweight, linetype
├── blocks/                     # Library blocks, dynamic blocks, attributes
├── dims/                       # Linear, angular, radial, baseline, continue
├── sheets/                     # Title blocks, viewports, sheet sets
├── command_line/               # Keyboard-first command parser
├── dxf/                        # DXF read/write (ASCII; R12-compatible subset)
└── dwg/                        # Native pure-Rust DWG read/write (R12 → R2018; in-process)
```

### Performance design

- All primitives and edits flow through the same command engine as the Design module.
- The 2D CAD canvas uses wgpu with an orthographic camera; rendering is batched per layer.
- Snapping precomputes a spatial index for end/mid/center/intersection/perpendicular/tangent snaps.
- Constraints are solved incrementally; the solver runs in a worker thread.
- DXF is the canonical ASCII format; DWG round-trip uses the in-process native codec under `dwg/`, with version dispatch from R12 (AC1009) through R2018 (AC1032) and no external runtime dependency.

---

## 9.4 Native BIM engine

```
crates/aec_bim/src/
├── ifc/reader.rs                # Native STEP parser: schema detection (IFC2x3 / IFC4 / IFC4x3), streaming iterator, UTF-8-safe, multi-line records & comments
├── ifc/writer.rs                # Native STEP writer: GUID preservation, deterministic numbering, verbatim Pset round-trip (incl. PropertyValue::Other)
├── tessellator.rs               # Geometry tessellator: IfcExtrudedAreaSolid, IfcFacetedBrep, profile types (rectangle/circle/arbitrary), IfcBooleanClippingResult
├── classification.rs            # AI-assisted classification adapter
├── properties.rs                # Pset/Qto editing (incl. PropertyValue::Other for verbatim round-trip)
├── schedules/                   # Room / door / window / material schedules (mod + per-schedule files)
├── validation.rs                # Strict validation against schema and project rules
└── diff.rs                      # IFC diff at element + property level
```

### BIM cache strategy

```
IFC file → native STEP parse → AEC project graph delta →
  ├── Persist deltas as commands in the command engine
  ├── Store a per-element BIM cache (geometry hash, Pset hash, classification)
  └── Materialize fast IFC re-export from cache (skip re-tessellation when unchanged)
```

The BIM cache lets large IFC models open in seconds on re-open and lets export skip unchanged geometry.

---

## 9.5 Render worker

```
crates/aec_render/
├── benches/native_render.rs    # Criterion benchmark for the native CPU/GPU path tracer end-to-end
├── bvh.rs                      # SAH BVH2 builder + node layout (Tasks 1-2, Phase 9 PR1)
├── cameras.rs                  # CameraSnapshot, CameraStore, CameraJournal, preset thumbnails
├── denoise.rs                  # Edge-aware bilateral / NLM denoiser (Phase 9 PR2)
├── doctor.rs                   # Material check / diagnostics (missing texture, non-PBR, swapped channels)
├── final_render.rs             # Native final-render pipeline — CPU/GPU path tracer → tone-map → PNG
├── gpu_trace.rs                # wgpu compute path tracer + fallback to CPU rayon path tracer
├── history.rs                  # RenderHistory + compare(a, b) → CompareResult
├── intersect.rs                # Möller-Trumbore + stack-based BVH traversal (Phase 9 PR1)
├── job.rs                      # RenderJob, RenderJobStatus, walkthrough frame tracking, resume state
├── light_sampling.rs           # Sun / area / point / IES sampling with MIS (Phase 9 PR1)
├── lighting.rs                 # Lighting presets (WarmEvening/Daylight/Studio/...), IES profiles
├── material.rs                 # Principled BSDF (diffuse + GGX, Schlick Fresnel, Smith shadowing)
├── panorama.rs                 # Native 360° equirectangular panorama pipeline
├── path_trace.rs               # CPU megakernel path tracer + CameraProjection + AccumulationBuffer
├── preset.rs                   # Quick / Standard / High / Studio / Preview / Walkthrough / Panorama + recommend_preset(tier)
├── preview.rs                  # Native PBR raster preview pipeline (replaces eevee.rs)
├── queue.rs                    # Render queue (single, batch, matrix); resume on failure; governor-bounded concurrency
├── scene.rs                    # RenderScene, RenderCamera, RenderLight serialization
├── scheduler.rs                # Tile / adaptive scheduler with cancellation (Phase 9 PR2)
├── shaders/path_trace.wgsl     # WGSL compute kernel used by gpu_trace.rs
└── walkthrough.rs              # Native walkthrough pipeline + optional ffmpeg stitching + WalkthroughOutput enum
```

> The file inventory above is the post-Phase-9 layout: `worker.rs`,
> `blender_discovery.rs`, `cycles.rs`, `eevee.rs`, and the entire
> `workers/blender/` and `workers/ifc/` trees were removed in Phase 9.
> Rendering and IFC handling are now fully in-process Rust.

---

## 9.6 KChat integration

KChat integration is the **only** optional cross-organisation surface in AEC Studio. It is gated on two axes:

1. **Compile-time** by the `kchat` cargo feature on `aec_core` (default-enabled). Disabling default features strips the `kchat`, `kchat_config`, and `kchat_sync` modules and their re-exports from the compiled crate; the `ActorKind::KChat` enum variant stays unconditional so the audit-log type remains stable across feature configurations.
2. **Runtime** by the `KChatConfig::enabled` toggle exposed in Settings. When the config is disabled, every publish/sync method returns `KChatError::KChatDisabled` and the corresponding UI elements hide themselves.

```
crates/aec_core/
├── kchat.rs                    # KChatArtifact (incl. AssetPack variant), ArtifactCard, KChatPublisher
│                               # trait, InMemoryPublisher (tests), ReviewComment / ApprovalStatus /
│                               # ReviewCard, AssetPackReference + AssetPackManifest::artifact_card
├── kchat_sync.rs               # One-way comment sync (KChat thread → audit trail) with dedup by
│                               # (thread_id, timestamp, commenter) so re-imports are no-ops
└── kchat_config.rs             # Local-first config (enabled: bool, default_thread_id: Option<String>);
                                # `KChatIntegration` gates every operation on `enabled`. Its
                                # `publish_asset_pack` routes the manifest *through* the transport as
                                # an asset-pack ArtifactCard and returns `AssetPackPublishOutcome`
                                # (reference + PublishResult).
```

```
apps/desktop/renderer/src/components/kchat/
├── PublishCardModal.tsx        # Preview an ArtifactCard before publishing
└── ArtifactCardPreview.tsx     # Image + caption + metadata preview tile
```

### Data flow

```
┌──────────────────────────────┐
│  AEC Studio (local-first)    │
│                              │
│  ┌────────┐    ┌──────────┐  │           ┌─────────────────┐
│  │ Render │───▶│ Artifact │──┼──publish──▶│  KChat thread   │
│  │  Sheet │    │   Card   │  │  (one-way) │  (user owned)   │
│  └────────┘    └──────────┘  │           └────────┬────────┘
│                              │                    │ inline comments
│  ┌──────────────────────┐    │                    ▼
│  │ AuditEntry           │◀───┼──ingest_review─── ReviewComment
│  │  actor.kind=KChat    │    │  (kchat_sync.rs, dedup'd)
│  └──────────────────────┘    │
└──────────────────────────────┘
```

- **Outbound only.** Project data never leaves AEC Studio except through an explicit publish. `KChatPublisher` is a trait; the user supplies the transport (HTTP, IPC, file drop) — there is no centralised AEC Studio service.
- **Inbound is comments only.** `kchat_sync.rs` imports `ReviewComment`s and turns them into `AuditEntry` rows with `ActorKind::KChat`. No project blobs are downloaded; team asset packs use the user's own transport for blobs and only sync manifest hashes.
- **Local-first guarantee.** Tests in `crates/aec_core/src/kchat_config.rs` confirm that a disabled config rejects every publish/sync method. The Settings page exposes the toggle so users can keep AEC Studio entirely offline.

---

### Camera and render state

- `CameraSnapshot` captures position, target, up, focal length (mm), sensor size, exposure (EV),
  white balance (K), depth of field (f-stop + focus distance), and aspect ratio.
- `CameraStore` is the source of truth for saved cameras; every mutation flows through
  `CameraJournal`, which folds into the global command engine for undo/redo.
- Thumbnails are rendered deterministically (32×32 RGBA8) from the snapshot so the camera tile
  grid is reproducible across machines.
- Camera presets (`InteriorCloseUp`, `Wide`, `EyeLevel`, `BirdsEye`) configure focal/sensor/DoF
  parameters; render presets configure the engine path (rasterized preview vs path-traced final), sample count, and resolution.

---

## 10.1 Resource governor

```
crates/aec_governor/
├── profiler/                   # Hardware profile (CPU, RAM, GPU, OS, accelerators)
├── policy/                     # Tier policies (low / medium / high / pro)
├── scheduler/                  # Render queue, AI queue, worker rate-limiting
├── thermal/                    # Backs off under sustained CPU/GPU load
├── memory/                     # Pressure-aware mesh / cache eviction
└── ui_report/                  # Surfaces governor state to the status bar
```

### What the governor controls

- Render preset and sample count.
- Number of concurrent path-tracer tiles.
- AI model tier (Bonsai 1.7B / 4B / 8B).
- Whether the PBR preview runs at full or half resolution.
- Whether the 3D viewport runs at native or downscaled framebuffer under heavy load.
- Eviction policy for the geometry mesh cache.
- Maximum simultaneous render jobs (1 by default on low-tier hardware).
- Background AI tasks while a render is running (paused by default).

---

## 10.2 Hardware profiles

| Profile | CPU | RAM | GPU | Default model tier | Default render preset |
|---|---|---|---|---|---|
| **Low** | Dual-core / 4-thread x86 or older M1 | 4–6 GB | None / integrated | Bonsai 1.7B Q4_K_M | PBR preview only, path tracer "Quick" (CPU fallback) |
| **Medium** | Modern 6-core x86 or M2 | 8–12 GB | Integrated or low-end discrete | Bonsai 1.7B / 4B | Path tracer "Standard" |
| **High** | 8+ cores or M2 Pro / M3 | 16–32 GB | RTX 3060 / Apple GPU 10-core+ | Bonsai 4B | Path tracer "High" |
| **Pro** | 12+ cores / Threadripper / M3 Max | 32+ GB | RTX 4070+ or Apple GPU 30-core+ | Bonsai 8B | Path tracer "Studio" |

The profile is detected at first run and re-evaluated when the user changes the runtime configuration. Users can override the recommended tier but the governor logs and surfaces the override.

---

## 10.3 Scene complexity budgets

| Scene type | Triangle budget | Asset instances | Lights | Notes |
|---|---|---|---|---|
| Single room | 0.5 M | 50–100 | 4–8 | Apartment / kitchen / bathroom |
| Full apartment | 2 M | 200–500 | 12–24 | Multi-room |
| Café / retail fit-out | 3 M | 500–1500 | 16–32 | Customer + back-of-house |
| Office floor | 5 M | 2000+ | 32–64 | Open plan + meeting rooms |
| Villa | 8 M | 3000+ | 48+ | Multi-storey |

Budgets are advisory — exceeding them triggers governor warnings and LOD swap-ins.

---

## 10.4 Viewport optimization

| Concern | Strategy |
|---|---|
| Mesh count | Per-asset LOD chain (LOD0 hero, LOD1 mid, LOD2 silhouette) |
| Draw call count | Per-layer batching in wgpu, instanced draws for furniture |
| Texture memory | Streaming texture atlas, lazy load per visible asset |
| Snapping cost | Precomputed snap index per element, refreshed on edit |
| Selection overhead | Stencil-based outline; no per-vertex CPU work |
| Idle GPU usage | Frame coalescing: viewport idles at 0 fps when nothing changes |
| Pan/zoom | Camera-space culling + frustum cull + occlusion query (where supported) |
| Hover queries | Spatial BVH built once per geometry-edit transaction |

---

## 10.5 Render optimization

### Render routing

```
User clicks "Render"
  │
  ├── If preview → PBR rasterizer pipeline (fast, denoise-less)
  ├── If final / batch / walkthrough → Path tracer pipeline
  │     │
  │     ├── Choose tile size from governor
  │     ├── Choose sample count from preset
  │     ├── Enable denoiser (OIDN / OptiX) per hardware
  │     └── Stream tiles back to the render queue
  └── Resume on failure via persisted job state
```

### Render job optimization

| Concern | Strategy |
|---|---|
| Cold-start | Reuse the BVH across jobs; rebuild only on geometry change |
| Mesh re-translation | Cache the path-tracer scene per project, invalidate by geometry hash |
| Sample budget | Per-preset minimum samples, denoise pass closes the rest |
| Multi-job throughput | Path-tracer tile concurrency capped to N tiles by the governor |
| Crash isolation | Worker crash never crashes AEC Studio; job marked failed and resumable |
| Walkthrough | Frame-by-frame resume from the last completed frame |

### User-facing render presets

| Preset | Engine | Samples | Denoise | Resolution scale | Notes |
|---|---|---|---|---|---|
| Quick | Path tracer | 32 | Bilateral / NLM | 0.75× | Fast looks |
| Standard | Path tracer | 128 | Bilateral / NLM | 1.0× | Daily delivery |
| High | Path tracer | 256 | Bilateral / NLM | 1.0× | Client hero |
| Studio | Path tracer | 1024 | Bilateral / NLM | 1.0× | Print-quality |
| Realtime Preview | PBR rasterizer | n/a | — | 0.5–1.0× | Real-time-ish viewport |
| Walkthrough | Path tracer | 96 / frame | Bilateral / NLM | 1.0× | Multi-frame |
| Panorama | Path tracer (equirectangular) | 512 | Bilateral / NLM | 1.0× | 360° room shots |

---

## 10.6 Asset optimization

### Pipeline

```
Asset import (glTF / FBX / OBJ)
  ├── Validate license + manifest
  ├── Normalize transform + units
  ├── Compute LOD chain (LOD0 hero, LOD1 mid, LOD2 silhouette)
  ├── Generate thumbnail (PBR rasterized quick render)
  ├── Hash mesh + materials (BLAKE3) for dedup
  └── Persist to the asset DB (SQLite + content-addressed blob store)
```

### Asset metadata

```json
{
  "asset_id": "asset_sofa_modern_3seat_v2",
  "name": "Modern 3-seat sofa",
  "license": "CC-BY-4.0",
  "attribution": "Studio AEC, 2026",
  "tags": ["sofa", "living", "modern", "fabric"],
  "style_tags": ["Scandinavian", "Japandi"],
  "lods": [
    { "level": 0, "tri_count": 24800, "blob_hash": "blake3:..." },
    { "level": 1, "tri_count":  6200, "blob_hash": "blake3:..." },
    { "level": 2, "tri_count":  1400, "blob_hash": "blake3:..." }
  ],
  "materials": [
    { "name": "Body fabric",   "pbr_hash": "blake3:..." },
    { "name": "Wood legs",     "pbr_hash": "blake3:..." }
  ],
  "thumbnail_blob": "blake3:...",
  "vendor": "studio-aec",
  "version": "2"
}
```

---

## 10.7 CAD optimization

| Concern | Strategy |
|---|---|
| Large drawings (100 k+ entities) | Spatial index (R-tree) per layer, viewport-clipped rendering |
| Live edit responsiveness | Constraint solver runs incrementally on background thread |
| Hatch fill cost | Pre-tessellated hatch cache invalidated only on boundary edits |
| Block instancing | One geometry blob + many transforms (GPU instancing) |
| DXF import | Streaming parser; entities materialized as they parse |
| Dim style recompute | Cached per dim style; only re-render dims that changed |
| Sheet plot | Background render thread, deterministic PDF output |
| Command line responsiveness | All commands are pure functions over the command engine |

---

## 10.8 BIM / IFC optimization

| Concern | Strategy |
|---|---|
| Large IFC parse | Native `IfcReader::iter` streaming iterator over `StepRecord`s; materialize geometry lazily via the in-process tessellator |
| Re-import speed | BIM cache keyed by element GUID + geometry hash |
| Pset edit cost | Element-level Pset diff, write-back only on save |
| Schedule generation | Indexed property store; schedule queries hit SQLite directly |
| Diff between models | Hash-based element diff + property diff (no full re-parse) |
| Validation | Rule-based engine + opt-in MVD; runs on background thread |
| Roundtrip | Preserve unknown Psets verbatim; never silently drop attributes |

---

## 10.9 Local AI optimization

| Concern | Strategy |
|---|---|
| Cold start | Reuse the llama.cpp / PrismML sidecar across requests (60 s idle-unload) |
| Memory | Model tier matched to the hardware profile |
| Throughput | `--parallel 2` shared sidecar, per-request cancellation |
| Latency | Grammar-constrained decoding emits only valid tool-call tokens |
| Quality | Bonsai 1.7B for simple tools; auto-upgrade to 4B/8B for planning |
| Power | Mac thermal back-off; Windows GPU split between viewport and inference |
| Determinism | Fixed seeds and deterministic samplers when reproducibility is required |
| Safety | All outputs pass through the safety validator before any commit |

---

## Local model runtime

### PrismML llama.cpp fork

AEC Studio uses the **PrismML** fork of llama.cpp ([kennguy3n/llama.cpp@prism](https://github.com/kennguy3n/llama.cpp)) for local model inference. The PrismML fork adds:

- Q1_0_g128 ternary repack format for memory-efficient small models.
- Acceleration across CUDA, Metal, Vulkan, AVX-512 VNNI, AVX-VNNI, AVX2, and ARM NEON.
- Tooling and packaging hooks AEC Studio uses to produce per-platform sidecars.

### Adapter bootstrap priority

```
MLXAdapter (macOS Apple Silicon) → LlamaCppAdapter (Windows / Linux / fallback) → Disabled (no AI mode)
```

### Inference tasks for AEC

| Task | Mode | Notes |
|---|---|---|
| Plan detection | Design | Vision model; outputs wall polylines |
| Style assistant | Design | Text + light vision; outputs furniture set + lighting tweak |
| Layout suggestion | Design | Text + spatial features; outputs alternate furniture layouts |
| Render doctor | Render | Vision; outputs preset and lighting suggestions |
| CAD cleanup | Draft | Tool-call planner over the 2D primitives |
| Plan-to-wall | Draft | Vision; outputs polylines for parametric walls |
| Schedule fill | Draft / BIM | Text; outputs Pset values |
| Classification | BIM | Vision + text; outputs IfcClass per element |
| Property fill | BIM | Text; outputs Pset/Qto values |
| Validation help | BIM | Text; outputs suggested fixes |
| Cover-page draft | Deliver | Text; writes a short concept paragraph |

### Shared sidecar pattern

Single `llama-server` (PrismML) process per session with:

- `--parallel 2` for two concurrent requests.
- `mmap` for fast model load.
- 60-second idle unload to reclaim memory.
- Per-request cancellation through the AI panel and command bar.

### Grammar-constrained decoding

All tool-call outputs are constrained with **GBNF** grammars in `crates/aec_ai/grammars/`. This ensures the planner cannot emit malformed JSON or unknown tool names. The safety validator then enforces scope and bounded changes before any preview diff is constructed.

### Device tiering

| Tier | Available RAM | Capability |
|---|---|---|
| **Low** | 4–6 GB | Bonsai 1.7B Q4_K_M only, no concurrent AI + render |
| **Medium** | 8–12 GB | Bonsai 1.7B / 4B, AI + PBR preview concurrently |
| **High** | 16–32 GB | Bonsai 4B always-on, AI + path tracer concurrent (with governor) |
| **Pro** | 32+ GB | Bonsai 8B always-on, multi-job AI + render |

---

## Project file structure

A project is a directory package with a `.aecstudio` extension. It is portable, content-addressed where possible, and safe to put under user-level sync (Dropbox, OneDrive, etc.).

```
project.aecstudio/
├── manifest.json               # Project id, schema version, units, region, standards
├── project.sqlite              # SQLCipher-encrypted project DB
├── commands/                   # Append-only command log (one file per day)
│   ├── 2026-05-19.jsonl
│   └── 2026-05-20.jsonl
├── checkpoints/                # Snapshots for fast load and rollback
│   ├── 2026-05-19T12-00.snap
│   └── 2026-05-20T09-00.snap
├── geometry/                   # Content-addressed mesh blobs
├── materials/                  # PBR material packs used by this project
├── assets/                     # Asset references (manifests) — actual blobs live in the asset DB
├── sheets/                     # 2D CAD sheet definitions
├── bim/                        # IFC import cache and pset overrides
├── renders/                    # Render queue state + finished image outputs
├── revisions/                  # Tagged revision snapshots
├── audit/                      # Append-only audit log (signed hash chain)
├── ai/                         # AI action log + accepted/rejected diffs
└── exports/                    # Last-known exports (PDF, DXF, IFC, glTF, packs)
```

### 8.1 Local database

The encrypted `project.sqlite` stores:

- The project graph as fast-lookup tables (entities, components, relations).
- Indexed properties for schedules and BOQ queries.
- The asset reference table (per-project — blobs live in the user-level asset DB).
- The undo journal head and last applied command id.
- Per-element BIM cache hashes.
- Audit and AI action metadata (full log bodies are append-only on disk in `audit/` and `ai/`).

Encryption uses SQLCipher with **AES-256 page-level** and per-project keys. Content hashing across the package uses **BLAKE3**. Per-scope DEKs follow the same cryptographic-forgetting pattern as [kennguy3n/knowledge](https://github.com/kennguy3n/knowledge): deleting the scope key permanently invalidates the data.

---

## Platform-specific notes

### macOS

| Component | Detail |
|---|---|
| Shell | Electron + React |
| Native addon | Universal N-API addon (Intel + Apple Silicon) |
| Preferred AI runtime | MLX (MLXAdapter) |
| Render GPU | wgpu Metal backend (native path tracer + PBR rasterizer) |
| Fallback AI runtime | LlamaCppAdapter (CPU AVX2/AVX-VNNI on Intel; CPU NEON on Apple Silicon) |
| Packaging | electron-builder, `.dmg` and `.zip` |
| Code signing / notarization | Apple Developer ID + notarytool |

### Windows

| Component | Detail |
|---|---|
| Shell | Electron + React |
| Native addon | C++ N-API addon |
| AI runtime | LlamaCppAdapter |
| CPU-only | AVX2 minimum, AVX-VNNI / AVX-512 VNNI when available |
| CPU+GPU | Vulkan / CUDA backend for inference; native wgpu path tracer on Vulkan / DX12 / Metal |
| Render GPU | wgpu D3D12 (default) or Vulkan |
| Packaging | electron-builder, `.exe` (NSIS) and `.msi` |
| Code signing | EV code-signing cert via Authenticode |

### Linux

| Component | Detail |
|---|---|
| Shell | Electron + React |
| Native addon | N-API addon (x86_64) |
| AI runtime | LlamaCppAdapter |
| CPU-only | AVX2 minimum, AVX-VNNI / AVX-512 VNNI when available (surfaced by `aec_governor::profiler`) |
| CPU+GPU | Vulkan for inference; native wgpu path tracer on Vulkan / DX12 / Metal |
| Render GPU | wgpu Vulkan backend |
| GPU detection | `/proc/driver/nvidia/version` → `lspci -mm` → `vulkaninfo --summary` (best-effort cascade) |
| Packaging | electron-builder, AppImage + `.deb`, optional Snap (`packaging/linux/`) |
| Desktop integration | `.desktop` file with `application/x-aec` MIME and `x-scheme-handler/aec` deep-link handler |

### Device tiering

| Tier | Available RAM | Capability |
|---|---|---|
| **Low** | 4–6 GB | Bonsai 1.7B Q4_K_M only, PBR preview, single render job |
| **Medium** | 8–12 GB | Bonsai 1.7B / 4B, path tracer "Standard" |
| **High** | 16–32 GB | Bonsai 4B always-on, path tracer "High" with GPU denoise |
| **Pro** | 32+ GB | Bonsai 8B always-on, path tracer "Studio", multi-job render queue |

---

## Security and privacy

| Principle | Implementation |
|---|---|
| Local-first storage | Project graph, assets, models, and renders live on the user's machine |
| Explicit network use | No telemetry; sync and KChat publish are explicit user actions |
| Encrypted project DB | SQLCipher with AES-256 page-level encryption and per-project keys |
| BLAKE3 content hashing | Used across geometry blobs, assets, audit log, command journal |
| Safe renderer | No direct file, worker, or model access from the renderer |
| Secure IPC | Typed and validated messages between renderer, main, and Rust core |
| Process separation | Renderer / main / Rust core are separate processes; AI sidecar is the only external worker. Rendering and IFC parsing are in-process Rust. |
| Worker sandboxing | Workers run with reduced privileges and explicit project-scoped file access |
| AI safety | Strict tool schema, grammar-constrained decoding, safety validator, preview diffs |
| Audit log | All commands, AI actions, exports, and connector events are logged |
| Cryptographic forgetting | Per-scope DEKs; deleting a key permanently invalidates the scope |
| Extension sandbox | Extensions declare permissions up front; the loader enforces them |

---

## Repository layout

```
aec-studio/
├── apps/
│   └── desktop/
│       ├── electron/           # Electron main process (main.ts, preload.ts, ipc.ts)
│       └── renderer/           # React / TypeScript UI (pages, components, hooks, styles)
├── crates/                     # Rust core engine
│   ├── aec_core/               # Core types, config, errors, project graph,
│   │                             revision/version_diff, KChat (kchat.rs, kchat_sync.rs, kchat_config.rs)
│   ├── aec_bridge/             # N-API bridge for Electron
│   ├── aec_command/            # Command engine, undo/redo journal
│   ├── aec_geometry/           # Geometry index, spatial queries, mesh cache
│   ├── aec_viewport/           # wgpu viewport, 2D CAD canvas, selection overlays, reference_image.rs
│   ├── aec_cad/                # 2D CAD: primitives, layers, blocks, snaps, dims
│   ├── aec_bim/                # Native BIM: STEP reader/writer, tessellator, spatial hierarchy
│   ├── aec_render/             # Native path tracer + PBR preview + walkthrough/panorama,
│   ├── aec_assets/             # Asset database, import pipeline, LOD, thumbnails
│   ├── aec_materials/          # PBR material library, texture management, mood_board.rs
│   ├── aec_ai/                 # AI command planner, tool schema, safety validator,
│   │                             layout_suggestion.rs, cover_draft.rs, lighting_balance.rs,
│   │                             schedule_fill.rs, validation_help.rs
│   ├── aec_governor/           # Resource governor, hardware profiler, scheduling,
│   │                             Linux soak test (tests/linux_soak.rs)
│   ├── aec_export/             # PDF, DXF, IFC, glTF, proposal pack export;
│   │                             boq.rs, interior_pack.rs, contractor_pack.rs, bim_pack.rs,
│   │                             before_after.rs, xlsx.rs; determinism + contractor_perf + phase6_e2e tests
│   └── aec_audit/              # Audit trail, project history (ActorKind::KChat for review comments)
├── workers/                    # Sidecar processes
│   └── ai/                     # llama-server sidecar config (the only remaining external sidecar)
├── templates/                  # Project, room, drawing, render, BIM templates
│   ├── interior/
│   ├── architecture/
│   └── drafting/
├── assets/                     # Bundled asset packs
│   ├── furniture/
│   ├── materials/
│   └── presets/
├── packaging/                  # electron-builder configs
│   ├── linux/                  # AppImage, .deb, .snap + .desktop
│   ├── macos/
│   └── windows/
├── docs/                       # Additional documentation
├── .github/workflows/          # CI configuration
├── LICENSE                     # AGPL-3.0
├── README.md
├── CONTRIBUTING.md
├── SECURITY.md
├── PROPOSAL.md
├── ARCHITECTURE.md
└── PROGRESS.md
```

---

## Design system

AEC Studio's UI follows the **KChat design system** (same tokens as [kennguy3n/Tessera](https://github.com/kennguy3n/Tessera)).

| Token | Value |
|---|---|
| **Primary accent** | `#7C3AED` (Purple/Violet) — headlines, CTA buttons, active states, links, icons |
| **Primary hover** | `#6D28D9` (darker violet) |
| **Background – page** | `#FFFFFF` (white) |
| **Background – card/surface** | `#F5F3FF` (light lavender) or `#F9FAFB` (light gray) |
| **Text – headline** | `#111827` (near-black) |
| **Text – body** | `#4B5563` (dark gray) |
| **Text – secondary** | `#6B7280` (medium gray) |
| **Font family** | `Inter` (primary), system sans-serif fallback stack: `-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif` |
| **Primary button** | Solid `#7C3AED` background, white text, pill/rounded shape (`border-radius: 9999px`) |
| **Secondary button** | Outlined with `#111827` border, dark text, uppercase tracking |
| **Cards** | White `#FFFFFF` background, `border-radius: 12px`, subtle shadow `0 1px 3px rgba(0,0,0,0.1)` |
| **Overall feel** | Clean, modern, minimal — purple dominant against white/light surfaces |

---

## Links

- [README.md](README.md) — project overview
- [PROPOSAL.md](PROPOSAL.md) — product proposal
- [PROGRESS.md](PROGRESS.md) — phased delivery tracker
- [PHASES.md](PHASES.md) — top-line phase status
- [EXTENSIONS.md](EXTENSIONS.md) — extension system: manifest schema, permissions, signatures
- [CONTRIBUTING.md](CONTRIBUTING.md) — contribution guide
- [SECURITY.md](SECURITY.md) — security policy
- [kennguy3n/llama.cpp@prism](https://github.com/kennguy3n/llama.cpp) — local AI inference
- [kennguy3n/IfcOpenShell](https://github.com/kennguy3n/IfcOpenShell) — reference implementation studied for the native Rust STEP parser (not a runtime dependency)
- [kennguy3n/cycles](https://github.com/kennguy3n/cycles) — path-traced renderer
- [kennguy3n/knowledge](https://github.com/kennguy3n/knowledge) — local knowledge substrate
- [kennguy3n/Tessera](https://github.com/kennguy3n/Tessera) — reference desktop architecture
