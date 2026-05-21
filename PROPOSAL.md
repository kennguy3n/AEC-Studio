# AEC Studio — Product Proposal

---

## Positioning

**General:**
> AEC Studio is a local-first open-source desktop suite for architecture, interior design, and construction — covering the daily production loop from plan to model to render to draft to deliver.

**Shorter:**
> AEC Studio is the local-first studio for small architecture and interior design teams that need to model, draft, and render real projects end to end.

**Developer-facing:**
> AEC Studio is an open-source Electron + Rust desktop suite combining a wgpu viewport, a 2D CAD module, BIM Lite via a native Rust IFC engine (STEP reader/writer + tessellator), a native Rust path tracer + PBR preview renderer (wgpu compute, CPU fallback), and a local AI assistant served by a llama.cpp / PrismML sidecar.

---

## Product direction

### What users can do

- Start a new project from an opinionated template (apartment, café, office, villa, retail, kitchen, bathroom, renovation).
- Model 3D spaces with walls, floors, ceilings, doors, and windows in the Design mode.
- Place furniture, finishes, and lighting from the bundled asset library or imported asset packs.
- Produce real construction drawings — plans, sections, elevations, details, and printable sheets — in the Draft mode.
- Import, view, classify, lightly edit, and re-export IFC building models in the BIM mode.
- Render previews via the native PBR rasterizer and final stills/walkthroughs/panoramas via the in-process Rust path tracer.
- Run local AI assistants (plan detection, style ideas, render doctor, CAD cleanup, BIM classification) entirely on-device.
- Bundle the same project into client proposals, contractor packs, and BIM exports without duplicating data.

### What AEC Studio is not

- **Not a clone of AutoCAD, Revit, or SketchUp** — focused on the production loop of small studios, not a general-purpose CAD/BIM platform.
- **Not a cloud-dependent SaaS** — every project, render, and inference runs on the user's machine. No cloud backend, no telemetry, no remote rendering.
- **Not a general chatbot** — AI is scoped to design, drafting, and BIM tools through a strict tool schema and a safety validator.
- **Not a real-time game engine** — the viewport prioritizes accuracy and snapping; final rendering uses the in-process Rust path tracer.

### Core promise

> AEC Studio turns templated rooms, plans, and BIM models into client renders, drawing sets, and contractor packages — all on your own machine, with AI as an inspectable assistant.

---

## Core product surfaces

| Surface | Purpose |
|---|---|
| **Home** | Project dashboard, templates, recents, asset library entry, hardware profile summary |
| **Design** | 3D space modeling, furniture, materials, lighting, cameras, design AI |
| **Draft** | 2D CAD canvas, layers, blocks, dims, sheets, title blocks, drafting AI |
| **BIM** | IFC import/export, spatial hierarchy, properties, schedules, takeoff, BIM AI |
| **Render** | PBR rasterized preview, path-traced final, batch queue, presets, walkthrough, panorama, render doctor — all native Rust |
| **Deliver** | Proposal pack, contractor handoff, BOQ-lite, IFC pack, PDF/DXF, revisions |

There is **no primary chat surface**. AEC Studio is a mode-based workflow application.

---

## Main workflow

```
Plan → Model → Furnish → Render → Draft → Quantify → Export → Revise
```

---

## UX principles

| # | Principle | What it means in practice |
|---|---|---|
| 1 | **Template-first** | Every project starts from a real template with rooms, walls, lighting, and an asset shelf. Empty file is the exception. |
| 2 | **Mode-based** | Home / Design / Draft / BIM / Render / Deliver are the top-level modes. The toolbar, inspectors, and AI panel reshape per mode. |
| 3 | **Progressive disclosure** | Beginner controls are visible by default. Snaps, constraints, command line, render presets, and governor overrides are reachable but not in the way. |
| 4 | **One project, many outputs** | A single `.aecstudio` package feeds renders, drawings, BIM exports, and contractor handoff packs. No re-modeling. |
| 5 | **AI is inspectable** | Every AI action surfaces as a previewable diff, gated by a tool schema and recorded in the audit trail. |
| 6 | **Performance is visible** | The hardware profile, GPU/CPU usage, governor state, and render queue are first-class UI, never hidden. |
| 7 | **Local-first** | No cloud account, no telemetry, no remote inference. Data and models live on the user's machine. |

---

## Technical principles

| # | Principle | Why |
|---|---|---|
| 1 | **Rust core, Electron shell, wgpu viewport** | Memory safety + fast indexing in Rust; mature UI in Electron/React; cross-platform GPU through wgpu. |
| 2 | **Worker-isolated heavy lifting** | Rendering, IFC parsing, and CAD ops run in-process in Rust; only AI inference runs as a separate sidecar process (llama.cpp / PrismML) for safety and parallelism. |
| 3 | **Typed IPC, no direct renderer access** | Renderer never touches files, tokens, or model binaries. All native capabilities flow through a typed N-API boundary. |
| 4 | **Command engine with undo/redo journal** | Every state mutation is a command, replayable and auditable. AI actions reuse the same engine. |
| 5 | **Resource governor with hardware tiers** | The governor reads a hardware profile, picks model sizes and render presets, and throttles workers under load. |
| 6 | **Open foundations** | Native Rust path tracer + IFC engine, llama.cpp/PrismML, and wgpu — all open-source, locally executable, license-compatible with AGPL-3.0. Reference implementations (kennguy3n/cycles, kennguy3n/IfcOpenShell) inform the design but are not runtime dependencies. |

---

## Suite-level information architecture

### Main desktop layout

```
┌──────────────────────────────────────────────────────────────────────────────┐
│  AEC Studio — [Project Name] · [Mode: Design]                       _  □  X  │
├──────────┬───────────────────────────────────────────────┬───────────────────┤
│          │                                               │                   │
│   Mode   │                                               │     Inspector     │
│   Rail   │              Active viewport                  │   (selection,     │
│          │       (3D / 2D CAD / IFC tree / Render)       │    material,      │
│ [Home]   │                                               │    properties)    │
│ [Design] │                                               │                   │
│ [Draft]  │                                               ├───────────────────┤
│ [BIM]    │                                               │                   │
│ [Render] │                                               │      AI Panel     │
│ [Deliver]│                                               │   (suggestions,   │
│          │                                               │   tool actions,   │
│          │                                               │   diff preview)   │
│          │                                               │                   │
├──────────┴───────────────────────────────────────────────┴───────────────────┤
│  Status bar: hardware profile · governor · render queue · audit · save state │
└──────────────────────────────────────────────────────────────────────────────┘
```

### Workflow modes

| Mode | Main canvas | Typical inspectors | AI panel emphasis |
|---|---|---|---|
| Home | Project dashboard + template gallery | Recent projects, asset library | Project starter suggestions |
| Design | 3D wgpu viewport | Object, material, lighting, camera | Plan detection, style assistant, layout suggestions |
| Draft | 2D wgpu CAD canvas | Layers, blocks, dim styles, sheet | CAD cleanup, plan-to-wall, schedule fill |
| BIM | 3D viewport + spatial tree | IFC properties, classification, schedules | Classification, property fill, validation |
| Render | Render preview surface | Camera, preset, lighting, queue | Render doctor, lighting tuning |
| Deliver | Pack composer | Export targets, revision, sheet set | Cover-page draft, schedule polish |

### Object model shared across modules

```
Project
├── Settings
│   ├── Units (mm / m / inches / feet)
│   ├── Region (EU / NA / APAC defaults)
│   ├── Standards (ISO, ANSI, custom)
│   └── Hardware profile (read-only)
├── Spatial graph
│   ├── Site
│   │   └── Building
│   │       └── Level
│   │           └── Space (Room)
│   └── References (north arrow, datum, levels)
├── Geometry
│   ├── Walls / Floors / Ceilings / Roofs
│   ├── Openings (doors, windows)
│   ├── Furniture instances
│   ├── Custom meshes
│   └── 2D drawings (linked to spaces and sheets)
├── Materials & finishes
├── Lighting & cameras
├── BIM properties (when present)
├── Sheets & drawing sets
├── Render queue & history
├── Asset references (local + project-bundled)
├── Audit trail (append-only)
└── AI actions log (gated by tool schema)
```

---

## User journeys

### A. Interior designer — "Apartment renovation in a weekend"

**Profile:** Solo interior designer, MacBook Pro M3, 18 GB RAM, working from a client's PDF floor plan.

**Goal:** Produce 4 client-ready renders and a furniture spec sheet within 2 days.

**Flow:**

1. **Home → New project → Apartment template (60 m²).** Picks region (EU), units (mm), and a "warm minimalist" preset.
2. **Design → Plan detection AI.** Drops the client's PDF onto the AI panel. The plan-detection model returns a previewed wall outline; she accepts the diff which becomes a parametric wall set.
3. **Design → Furnish.** Drags sofa, dining table, bed, lamps, and rugs from the bundled assets. Uses the material panel to swap finishes and recolor walls.
4. **Design → Lighting.** Picks "warm evening" lighting preset; AI suggests two extra accent lights, both shown as a preview before commit.
5. **Render → 4 cameras.** Saves 4 cameras (living, dining, bedroom, kitchen). Picks "Interior High" preset. Render queue runs the native path tracer in the background; PBR rasterized thumbnails are ready in <30 s each.
6. **Deliver → Client concept pack.** Generates a PDF with cover, mood board, floor plan, 4 renders, material schedule, and a "next steps" page. Saves a revision snapshot.

**UX details:**

- The plan-detection AI shows the proposed walls as a translucent ghost overlay; user accepts/edits before commit.
- The material library filters by style tag ("Scandinavian", "Industrial", "Japandi").
- The render queue surfaces per-job ETA, denoise pass, and resume on failure.

**Acceptance criteria:**

- [ ] Project goes from new-template to 4 final renders in < 2 hours of active work on a mid-tier Mac.
- [ ] Every render is reproducible from the saved camera + preset.
- [ ] Client pack exports as a single PDF with embedded schedule.

### B. Architecture studio — "Café fit-out with construction drawings"

**Profile:** 3-person studio, Windows 11 PCs with RTX 4070, working on a 120 m² café.

**Goal:** Deliver IFC export, 6 construction sheets, and 2 hero renders for the contractor and client.

**Flow:**

1. **Home → New project → Café template.** Two designers and a drafter open the same project package on three machines (local file sync via the user's preferred sync tool — Dropbox, OneDrive, USB drive).
2. **Design → Modeling.** Designers shape walls, banquettes, counters, lighting. The drafter starts on Draft mode in parallel.
3. **Draft → Sheets.** Drafter creates plan, section, and elevation sheets from the live model. Title block is the studio's saved template.
4. **BIM → Classification.** AI proposes IFC classes (IfcWall, IfcDoor, IfcFurniture). Drafter accepts via diff preview.
5. **BIM → Schedules.** Door schedule and room schedule generate from the spatial tree. AI fills missing fire ratings using the studio's project standards file.
6. **Render → 2 hero shots.** Native path tracer on wgpu compute, RTX 4070; ~6 minutes per hero render at 4K with denoise.
7. **Deliver → Contractor handoff pack.** Bundles sheets, schedules, IFC, BOQ-lite into a single zipped pack with a manifest.

**UX details:**

- IFC roundtrip preserves GUIDs so the model can be re-imported into Revit or ArchiCAD downstream.
- The drafter's sheet view updates live as the designers edit geometry.
- Sheet exports are deterministic — same model + same sheet = identical PDF.

**Acceptance criteria:**

- [ ] Construction sheets stay in sync with the 3D model; no manual re-tracing.
- [ ] IFC export validates against the native strict-mode parser and re-imports with full GUID match.
- [ ] Contractor pack export takes < 60 s on a mid-range PC.

### C. Construction PM — "Site renovation with BIM Lite and BOQ"

**Profile:** Construction project manager, ThinkPad with i7, 16 GB RAM, no GPU.

**Goal:** Import an existing IFC, classify missing elements, fix obvious issues, produce a room-by-room BOQ for the bid.

**Flow:**

1. **Home → Open project → Import IFC.** Drops a 40 MB IFC. The native STEP parser imports in a few seconds; spatial tree appears in the BIM mode.
2. **BIM → Validate.** Runs the AEC Studio validator: surfaces dangling references, missing IFC classes, and unclosed spaces.
3. **BIM → AI classification.** AI proposes IFC classes for unclassified meshes; PM accepts only the high-confidence ones (≥ 0.85) and reviews the rest manually.
4. **BIM → Property fill.** AI suggests property sets for walls and floors (material, fire rating, thickness) from project standards. Diff preview, accept or edit.
5. **BIM → Schedule + BOQ-lite.** Generates room schedule, door schedule, and quantity takeoff. Exports as XLSX.
6. **Draft → Drawing generation.** Generates plan and elevation sheets from the BIM model for inclusion in the bid.
7. **Deliver → BIM Lite export pack.** IFC + sheets + BOQ + validation report in one zipped pack.

**UX details:**

- CPU-only mode is fully supported. AI runs at Q4_K_M on a Bonsai 1.7B model and is responsive (~tokens/sec acceptable for tool actions).
- The validator's findings are clickable and jump to the offending element in the viewport.

**Acceptance criteria:**

- [ ] 40 MB IFC imports in < 15 s on the target hardware.
- [ ] BOQ-lite XLSX exports with at least 95 % of materials accounted for.
- [ ] AI classification confidence threshold is configurable.

### D. Drafter — "Pure 2D CAD for a steel detail set"

**Profile:** Senior drafter, Windows 10, 8 GB RAM, dual monitor.

**Goal:** Produce a 12-sheet steel detail set without touching the 3D module.

**Flow:**

1. **Home → New project → 2D Drafting template.** Skips Design mode entirely.
2. **Draft → DXF import.** Drops a vendor DXF; the CAD worker imports layers, blocks, and dim styles.
3. **Draft → Drawing tools.** Uses line, polyline, arc, circle, spline, hatch, text, dim, leader, fillet, chamfer, trim, extend. Snaps + ortho + polar tracking.
4. **Draft → Sheets.** Sets up 12 sheets with title blocks, viewports linked to detail blocks.
5. **Draft → Command line.** Uses keyboard-driven commands (`L` for line, `O` for offset, `CO` for copy) without leaving the keyboard.
6. **Deliver → DXF/DWG + PDF.** Exports the sheet set as multi-page PDF and a clean DXF; optional DWG via the converter adapter.

**UX details:**

- Command line is a first-class drafter affordance; keyboard shortcuts mirror QCAD/AutoCAD conventions.
- Layers and blocks roundtrip through DXF without loss.
- The DWG strategy is layered: native DXF is the canonical format; DWG goes through a converter adapter so AGPL-3.0 obligations stay clean.

**Acceptance criteria:**

- [ ] Drafter can complete a sheet set with keyboard-only workflows.
- [ ] DXF roundtrip preserves layer, block, dim style, and text style.
- [ ] DWG export is available but explicitly opt-in.

### E. Studio lead — "Concept to contract in one project package"

**Profile:** Studio principal coordinating designer, drafter, and PM on a villa.

**Goal:** Use a single project package for concept renders, construction set, BIM export, and contract documents.

**Flow:**

1. **Home → New project → Villa template.** Sets standards (regional building codes), units (mm), and the studio's title block.
2. **Design → Concept.** Designer models massing and main spaces; renders 3 concept images.
3. **Draft → Construction set.** Drafter produces 24 sheets from the live model.
4. **BIM → IFC classification.** Studio lead classifies model and produces an IFC for the structural engineer.
5. **Deliver → Revisions.** Each delivery produces a revision snapshot. Before/after compare shows the deltas in plan, model, and schedules.
6. **Deliver → Contract pack.** PDF + IFC + DXF + BOQ + proposal.

**UX details:**

- Revision system is built into the project package — no separate version-control workflow required.
- Before/after compare works on geometry, sheets, and schedules.
- Audit trail records who applied which AI action and when.

**Acceptance criteria:**

- [ ] One `.aecstudio` package feeds all four delivery types (renders, drawings, IFC, contract).
- [ ] Revisions can be diffed at the project, sheet, and element level.
- [ ] Studio standards (title block, dim style, layer policy) are stored in the project and reused across deliverables.

---

## Functional specification by module

### 5.1 ArchViz / Interior Studio

#### Core features

| Feature | Detail |
|---|---|
| Room modeling | Parametric walls, floors, ceilings, openings; snap-to-grid and snap-to-element |
| Door / window placement | Library-driven, swing/slide presets, automatic wall cut |
| Furniture browser | Tag-faceted browser (style, room, vendor, license) with drag-to-place |
| Material library | PBR materials, vendor packs, instance overrides, finish swap |
| Lighting | Sun + sky model, area lights, IES profiles, lighting presets |
| Cameras | Saved cameras with focal length, exposure, white balance, DoF |
| Snapshots | Camera + preset snapshots for reproducible renders |
| Reference images | Drop PDF/JPG into the viewport as a tracing reference |

#### Interior-specific generators

| Generator | What it produces |
|---|---|
| Plan-to-wall | Walls inferred from a PDF/raster plan via the plan-detection AI |
| Furniture suggest | Style-matched furniture layout proposals (preview before commit) |
| Lighting balance | Suggests fill/accent lights to match a reference mood |
| Mood board | Auto-generated swatches and material picks per room |
| Schedule fill | Furniture and material schedules generated from instances |

#### Render modes

| Mode | Engine | Use case |
|---|---|---|
| **Preview** | Native PBR rasterizer (wgpu) | Real-time-ish preview at viewport refresh rates |
| **Final** | Native path tracer (wgpu compute, CPU fallback) | Photoreal stills with denoise |
| **Batch** | Native render queue | Multi-camera, multi-preset batches |
| **Walkthrough** | Native path tracer + camera path | Short walkthrough animations |
| **Panorama** | Native equirectangular path tracer | 360° room panoramas for VR previews |

### 5.2 2D CAD Drafting

#### Core features

| Feature | Detail |
|---|---|
| Drawing primitives | Line, polyline, arc, circle, ellipse, spline, hatch, text, MText, dim, leader, table |
| Editing tools | Move, copy, rotate, scale, mirror, offset, trim, extend, fillet, chamfer, stretch |
| Precision tools | Grid, ortho, polar tracking, object snaps, tracking, parametric constraints |
| Layers | Layer state manager, freeze/thaw, color, lineweight, linetype |
| Blocks | Library blocks, dynamic blocks (parameter-driven), block attributes |
| Dimensions | Linear, angular, radial, baseline, continue; dim styles |
| Sheets | Title blocks, viewports, sheet sets, scale per viewport |
| Plot | Print to PDF, raster PDF for archival, plot styles |

#### CAD UX details

- A first-class **command line** at the bottom: `L`, `O`, `CO`, `MO`, `MI`, `TRIM`, `EX`, `F`.
- Mouse-driven workflow is also fully supported, but keyboard-first is canonical.
- Selection sets, named selections, and quick-select filters.
- Constraint solver for parametric details (matching FreeCAD-style 2D constraints).

#### DWG strategy

| Layer | Detail |
|---|---|
| **Canonical** | DXF — open spec, lossless roundtrip, AGPL-clean |
| **Compatibility** | DWG via a converter adapter — explicit opt-in, isolated process |
| **Long-term** | Native DWG read only when the user provides their own ODA Teigha (or equivalent) library; never bundled |

### 5.3 BIM Lite / IFC

#### Core features

| Feature | Detail |
|---|---|
| IFC import | Native Rust STEP parser, IFC2x3 + IFC4 + IFC4x3 |
| IFC export | Round-trippable with GUID preservation |
| Spatial hierarchy | Project / Site / Building / Storey / Space tree |
| Classification | IfcWall, IfcSlab, IfcDoor, IfcWindow, IfcFurniture, IfcRoof, etc. |
| Property editor | Pset_* and Qto_* sets, custom psets, type/instance properties |
| Schedules | Rooms, doors, windows, walls, materials |
| Takeoff | BOQ-lite per discipline (architecture, finishes) |
| Validation | Dangling refs, missing classes, unclosed spaces, duplicate GUIDs |
| Diff | Compare two IFC versions at element + property level |

#### BIM Lite scope boundaries

| In scope | Out of scope (for MVP) |
|---|---|
| IFC import/export and lightweight editing | Full MEP, structural design, parametric families |
| Classification and property editing | IFC validation against a project-specific MVD |
| Room, door, window, material schedules | Full COBie deliverables |
| BOQ-lite (areas, counts) | Full cost estimating with vendor pricing |
| Diff at element + property level | Federation across multi-discipline models |
| Drawing generation from BIM model | Construction sequencing / 4D |

---

## Local AI design

### AI product behavior

```
User intent → tool schema match → planner → safety validator →
preview diff → user confirm → command engine → audit log
```

The AI never mutates geometry, materials, schedules, or sheets without an explicit user confirmation on a preview diff.

### AI capabilities by workflow

| Workflow | Capability |
|---|---|
| Design | Plan detection from a raster/PDF plan |
| Design | Style assistant — propose furniture, finishes, lighting |
| Design | Layout suggestions — alternate furniture arrangements |
| Render | Render doctor — diagnose noise, lighting issues, suggest preset/sample changes |
| Render | Lighting tuning — match a target reference image |
| Draft | CAD cleanup — close gaps, remove duplicate lines, normalize layers |
| Draft | Plan-to-wall — turn a raster plan into parametric walls |
| Draft | Schedule fill — fill door/room schedules from element properties |
| BIM | Classification — propose IFC classes for unclassified meshes |
| BIM | Property fill — propose Pset values from project standards |
| BIM | Validation help — suggest fixes for validator findings |
| Deliver | Cover-page drafting — write a concept paragraph from project metadata |

### AI command schema

```json
{
  "tool": "design.place_furniture",
  "version": "1.0",
  "arguments": {
    "asset_id": "asset_sofa_modern_3seat_v2",
    "anchor": { "type": "room", "id": "space_living_room" },
    "offset_mm": { "x": 0, "y": 0, "z": 0 },
    "rotation_deg": 0,
    "scale": 1.0
  },
  "preview": {
    "diff_id": "diff_a1b2c3",
    "actions": [
      { "kind": "create", "entity": "furniture_instance" }
    ]
  },
  "safety": {
    "scope": "design",
    "requires_confirm": true,
    "max_entities_modified": 1
  }
}
```

### AI safety rules

| Rule | Implementation |
|---|---|
| No silent mutation | Every action surfaces as a previewable diff before commit |
| Tool schema only | The planner cannot emit any operation outside the registered tool schema |
| Scoped tools | Each tool is scoped to a mode (design / draft / bim / render / deliver) |
| Bounded changes | A tool declares `max_entities_modified`; the safety validator enforces it |
| Audit | Every accepted or rejected AI action is logged with diff hash, tool, scope |
| No exfiltration | AI cannot trigger export, sync, or network requests directly |
| Local-only | The model runs on the local llama.cpp / PrismML sidecar; no external API |
| Cancellable | All AI inference is cancellable from the AI panel and the command bar |

### Local AI runtime

```
aec_ai (Rust)
├── tool_schema/                # Tool definitions (typed JSON schema)
├── planner/                    # Schema-bound planner (grammar-constrained decoding)
├── safety_validator/           # Enforces scope, bounded changes, tool whitelist
├── diff_engine/                # Builds previewable diffs against the command engine
├── runtime/                    # Sidecar lifecycle (llama.cpp / PrismML / MLX)
├── grammars/                   # GBNF grammars for tool calls and structured outputs
└── audit/                      # Logs accepted/rejected actions to the audit trail
```

### AI model tiers

| Tier | Model | RAM target | Use case |
|---|---|---|---|
| **Lightweight** | Bonsai 1.7B (Q4_K_M or Q1_0_g128 ternary) | 2–4 GB | Tool calls, schedule fill, classification suggestions |
| **Balanced** | Bonsai 4B | 6–8 GB | Style assistant, longer planning, BIM property fill |
| **Higher quality** | Bonsai 8B | 10+ GB | Multi-step layout suggestions, render doctor with rich rationale |

On Apple Silicon, models run via **MLX** (2-bit or 4-bit). On Windows, they run via **llama.cpp / PrismML** (CPU AVX2/AVX-VNNI/AVX-512 VNNI, GPU Vulkan/CUDA).

---

## Template and asset layer

### Template types

| Template | Includes | Typical user |
|---|---|---|
| Apartment | Room shells, wall presets, asset shelf, lighting preset, sample camera | Interior designer |
| Café | Counter, banquette, table layout, lighting preset, signage block | Studio for fit-out |
| Office | Workstation layout, meeting rooms, lighting preset, partition library | Workplace designer |
| Villa | Multi-storey shell, garden block, full BIM tree | Architecture studio |
| Retail | Display fixtures, signage, lighting preset, customer flow | Retail designer |
| Kitchen | Cabinet library, appliance presets, plumbing notes, lighting preset | Kitchen specialist |
| Bathroom | Sanitary library, tiling presets, waterproofing notes | Bathroom specialist |
| Renovation | Existing-condition overlay, demo/keep/new layers, before/after camera pair | Renovator |

### Regional localization

- **EU** — millimetres, ISO sheet sizes (A0/A1/A2/A3/A4), EU door/window standards, EN materials.
- **NA** — feet/inches, ANSI sheet sizes (A/B/C/D/E), NA stud/door standards.
- **APAC** — millimetres, ISO sheet sizes, region-specific furniture and finishes.

### Asset pack structure

```
asset_pack/
├── manifest.json               # Pack id, version, license, attribution
├── thumbnails/                 # Pre-rendered thumbnails per LOD
├── meshes/                     # glTF / FBX-equivalent mesh data
├── materials/                  # PBR material definitions
├── lods/                       # Pre-computed LOD chains
└── presets/                    # Placement presets per template (apartment, office, ...)
```

---

## Extension system

### Extension types

| Type | What it can do |
|---|---|
| **Asset pack** | Adds furniture, materials, presets; metadata-only — no executable code |
| **Template** | Adds a new project template, including rooms, lighting, asset shelf |
| **Schedule** | Adds a new schedule definition (door, room, custom) |
| **Export target** | Adds a new export format (e.g., third-party BOQ XLSX dialect) |
| **AI tool** | Adds a new tool to the AI tool schema (must declare scope + safety bounds) |
| **Importer** | Adds a new importer (e.g., a vendor's proprietary plan format) |

### Extension permissions

```json
{
  "id": "ext.example.boq_xlsx",
  "name": "Vendor BOQ XLSX exporter",
  "version": "1.0.0",
  "type": "export_target",
  "permissions": {
    "filesystem_write": "deliver_output_only",
    "network": "none",
    "geometry_read": true,
    "geometry_write": false,
    "ai_tools": false,
    "audit_log": true
  },
  "signature": "ed25519:base64...",
  "license": "MIT"
}
```

Extensions are signed, declare their permissions up front, and run sandboxed. The renderer never loads extension code directly.

---

## Optional KChat integration

AEC Studio is local-first; KChat integration is **opt-in** and confined to publishing artifact cards into KChat threads.

### Publishable artifacts

| Artifact | What gets published |
|---|---|
| Concept render | Image + caption + project link |
| Sheet | PDF preview + sheet id |
| Revision pack | Bundle manifest + summary |
| BOQ snapshot | Top-line totals + link to full XLSX |

### KChat flow

```
User selects artifact → "Publish to KChat" → preview card → confirm → KChat thread
```

Publishing is one-way (AEC Studio → KChat). Comments and approvals can sync back as audit-trail entries, but no geometry or schedule is mutated by KChat actions.

---

## Licensing and compliance strategy

### License constraints

- **AEC Studio** itself is **AGPL-3.0**.
- **Cycles** ([kennguy3n/cycles](https://github.com/kennguy3n/cycles)) is Apache-2.0; AEC Studio studied its kernel for the native path tracer but does NOT link or invoke it at runtime.
- **IfcOpenShell** ([kennguy3n/IfcOpenShell](https://github.com/kennguy3n/IfcOpenShell)) is LGPL-3.0; AEC Studio studied its STEP parser as a reference but the native Rust parser in `aec_bim::ifc` is a clean implementation that does NOT link or invoke IfcOpenShell at runtime.
- **Blender** is GPL-3.0; AEC Studio does NOT bundle, link, or invoke Blender. All rendering is in-process Rust.
- **llama.cpp / PrismML** ([kennguy3n/llama.cpp](https://github.com/kennguy3n/llama.cpp)) is MIT; bundling and modifying is compatible.
- **wgpu**, **napi-rs**, **SQLite/SQLCipher**, and **Electron** licenses are reviewed and compatible.
- Bundled asset packs declare individual licenses; we accept only license-clean assets (CC0, CC-BY with attribution, or studio-original).

### Recommended compliance posture

| Concern | Posture |
|---|---|
| AGPL network-use obligation | Make the source available for any user who interacts with the application — README + LICENSE + a "Corresponding Source" link |
| GPL/AGPL contamination via Blender | Eliminated — Blender is no longer a runtime dependency |
| LGPL contamination via IfcOpenShell | Eliminated — IfcOpenShell is no longer a runtime dependency |
| Asset licensing | Per-asset `LICENSE` and attribution stored in the pack manifest; "Attributions" pane in Settings |
| Cryptographic export | SQLCipher is permitted; we publish the crypto algorithms used (XChaCha20-Poly1305 + AES-256 page-level) |
| Privacy | No telemetry, no network without explicit user action; documented in [SECURITY.md](SECURITY.md) |

---

## Main risks

| Risk | Mitigation |
|---|---|
| Render queue stalls on low-end hardware | Resource governor with auto-tuned preset, PBR-rasterizer-only fallback for low-tier, render resume on failure |
| AI hallucinates a destructive mutation | Strict tool schema, grammar-constrained decoding, safety validator, preview diffs, audit log |
| IFC roundtrip loses GUIDs or Psets | Native parser preserves GUIDs by construction; round-trip tests in CI; unknown property types preserved verbatim via `PropertyValue::Other` |
| DWG legal/license issues | DXF is canonical; DWG is opt-in through a separate converter adapter, never bundled by default |
| Native render engine regressions | Comprehensive workspace test suite covers BVH, BSDF, MIS, denoiser, scheduler, and PBR preview; benchmarks track regressions across releases |
| wgpu backend gaps on older GPUs | Detect at startup and fall back to a software OpenGL path with clear messaging |
| AGPL chilling effect on commercial adopters | Clear `Corresponding Source` link, separate commercial license SKU document if/when needed |
| Project file format churn | Versioned JSON schema, migrations crate, deterministic upgrades, snapshot before migrate |
| Asset library bloat | Lazy pack loading, per-pack LOD, on-demand download for optional packs |
| Local model size on small disks | Per-tier downloads, on-demand only, manifest with checksums and sizes |

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
- [ARCHITECTURE.md](ARCHITECTURE.md) — technical architecture
- [PROGRESS.md](PROGRESS.md) — phased delivery tracker
- [PHASES.md](PHASES.md) — top-line phase status
- [EXTENSIONS.md](EXTENSIONS.md) — extension system: manifest schema, permissions, signatures
- [CONTRIBUTING.md](CONTRIBUTING.md) — contribution guide
- [SECURITY.md](SECURITY.md) — security policy
- [kennguy3n/llama.cpp@prism](https://github.com/kennguy3n/llama.cpp) — local AI inference
- [kennguy3n/IfcOpenShell](https://github.com/kennguy3n/IfcOpenShell) — reference implementation for the native Rust STEP parser (not a runtime dependency)
- [kennguy3n/cycles](https://github.com/kennguy3n/cycles) — path-traced renderer
- [kennguy3n/knowledge](https://github.com/kennguy3n/knowledge) — local knowledge substrate
- [kennguy3n/Tessera](https://github.com/kennguy3n/Tessera) — reference desktop architecture
