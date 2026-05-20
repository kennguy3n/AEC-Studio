# AEC Studio Extensions

AEC Studio extensions let third parties contribute **asset packs, templates, schedules, export targets, AI tools, and importers** to the desktop app without modifying core code. Every extension is a directory containing a signed JSON manifest plus the payload files (meshes, JSON templates, scripts) it brings.

This document is the canonical spec. The Rust implementation lives in [`crates/aec_core/src/extensions.rs`](crates/aec_core/src/extensions.rs) (loader, registry, manifest schema) and [`crates/aec_core/src/extension_permissions.rs`](crates/aec_core/src/extension_permissions.rs) (validator, permission enforcer, Ed25519 verification). Per-type hosts live in:

| Type           | Host crate                                                                                       |
| -------------- | ------------------------------------------------------------------------------------------------ |
| `asset_pack`   | [`aec_assets::extension_host`](crates/aec_assets/src/extension_host.rs)                          |
| `template`     | [`aec_core::templates::TemplateLoader::load_with_extensions`](crates/aec_core/src/templates.rs)  |
| `schedule`     | [`aec_bim::schedules::extension_host`](crates/aec_bim/src/schedules/extension_host.rs)           |
| `export_target`| [`aec_export::extension_targets`](crates/aec_export/src/extension_targets.rs)                    |
| `ai_tool`      | [`aec_ai::extension_tools`](crates/aec_ai/src/extension_tools.rs)                                |
| `importer`     | manifest accepted; runtime host wires up at v1.1 (parsed by the loader today)                    |

---

## 1. Why extensions

The shipped feature surface — Design, Draft, BIM, Render, Deliver — covers ~90% of the workflow. The remaining 10% is where studios diverge: vendor asset libraries, regional schedule formats, branded PDF packs, AI tools tuned to a discipline. Extensions are the supported way to add that 10% without forking AEC Studio or shipping closed code into the core.

Concretely an extension can:

- Add **asset entries** (furniture, materials, presets) into the asset browser
- Add **project templates** discoverable from the Home page
- Add **custom schedules** (e.g. fire-rated door schedule, FF&E schedule)
- Add **custom export targets** in Deliver mode (e.g. an in-house contractor PDF)
- Add **AI tools** the planner can dispatch to, with their own safety envelope
- Add **importers** for vendor file formats

Each type is independently optional — an extension picks the body that matches its `type` field.

---

## 2. Directory layout

```
extensions/
└─ acme.fire_doors/
   ├─ manifest.json
   ├─ schedules/
   │  └─ fire_door_columns.json
   └─ assets/
      ├─ door_1h.glb
      └─ door_2h.glb
```

- The directory name does **not** participate in identity — the `id` field in `manifest.json` is the lookup key.
- Every payload path inside `manifest.json` is resolved relative to the extension root.
- The loader walks `extensions/*/manifest.json` once at startup (and again whenever a user toggles the directory in Settings).

---

## 3. Manifest schema

`manifest.json` is the only mandatory file. The full schema:

```jsonc
{
  "id": "acme.fire_doors",                     // stable kebab/dot id
  "name": "ACME Fire-rated Doors",             // display name
  "version": "1.2.0",                          // semver
  "type": "asset_pack",                        // one of: asset_pack | template | schedule | export_target | ai_tool | importer
  "permissions": [                             // see §5
    "filesystem_read",
    "geometry_read"
  ],
  "signature": {                               // optional; see §6
    "algorithm": "ed25519",
    "public_key_hex": "...",
    "signature_hex": "..."
  },
  "license": "AGPL-3.0",                       // SPDX identifier
  "description": "Fire-rated commercial doors with attached schedules.",

  // Exactly one of the type-specific bodies below is populated;
  // the others are null/absent.
  "asset_pack":   { "vendor": "ACME", "entries": [ /* AssetEntry */ ] },
  "template":     { "key": "interior.boutique_hotel",
                    "definition_path": "templates/boutique.json" },
  "schedule":     { "schedule_id": "acme.fire_door_schedule",
                    "display_name": "Fire-rated Doors",
                    "columns": [ /* ScheduleColumnDef */ ],
                    "formulas": [ /* ScheduleFormulaDef */ ] },
  "export_target":{ "target_id": "acme.contractor_pdf",
                    "display_name": "ACME Contractor PDF",
                    "format": "pdf",
                    "default_extension": "pdf",
                    "entry_path": "targets/contractor.lua" },
  "ai_tool":      { "tool_id": "acme.fire_classifier",
                    "display_name": "Fire-rating Classifier",
                    "description": "Classify doors by required fire rating.",
                    "allowed_scopes": ["bim"],
                    "max_entities_modified": 50,
                    "grammar_key": "classification" },
  "importer":     { "importer_id": "acme.skp",
                    "display_name": "SketchUp Importer",
                    "extensions": ["skp"],
                    "entry_path": "importers/skp.bin" }
}
```

The Rust types are in `aec_core::extensions`:
[`ExtensionManifest`](crates/aec_core/src/extensions.rs), [`AssetPackBody`](crates/aec_core/src/extensions.rs), [`TemplateBody`](crates/aec_core/src/extensions.rs), [`ScheduleBody`](crates/aec_core/src/extensions.rs), [`ExportTargetBody`](crates/aec_core/src/extensions.rs), [`AiToolBody`](crates/aec_core/src/extensions.rs), [`ImporterBody`](crates/aec_core/src/extensions.rs).

The manifest is canonicalized for signing via [`aec_core::canonical_payload_bytes`](crates/aec_core/src/extensions.rs): the `signature` field is stripped, every map is serialized with sorted keys, and the result is BLAKE3-stable across field order. This is the byte string the signer must sign and the verifier hashes.

---

## 4. Extension types

### 4.1 Asset pack (`type: "asset_pack"`)

Adds furniture / material / preset entries to the asset browser. Each entry references a file inside the extension dir and declares the BLAKE3 hash of that file's bytes; the host (`aec_assets::install_asset_packs`) reads the file, recomputes the hash, and **rejects** any mismatch — this is the integrity gate that prevents a malicious tampered payload from being silently swapped in after signing.

Asset packs are **idempotent**: re-running `install_asset_packs` against a registry that already has the assets installed reports them as `skipped`, never duplicates them.

Required permissions: `filesystem_read`, `geometry_read`.

### 4.2 Template (`type: "template"`)

Adds one project template under a `<category>.<id>` key (e.g. `interior.boutique_hotel`). The loader composes extension templates with the on-disk `templates/` tree via [`TemplateLoader::load_with_extensions`](crates/aec_core/src/templates.rs) and [`discover_with_extensions`](crates/aec_core/src/templates.rs). On key collision the extension wins, so studios can override a shipped template with a versioned, signed pack.

Required permissions: `filesystem_read`, `geometry_read`.

### 4.3 Schedule (`type: "schedule"`)

Adds a custom schedule sheet (columns + optional formulas) to the schedule UI. [`build_extension_schedule`](crates/aec_bim/src/schedules/extension_host.rs) returns a `ScheduleSheet` with one default-valued row per declared column so the UI can render the schedule before any project data is bound.

Required permissions: `geometry_read`.

### 4.4 Export target (`type: "export_target"`)

Adds a custom export target to Deliver mode. [`resolve_export_target`](crates/aec_export/src/extension_targets.rs) returns an `ExtensionExportTarget` descriptor including the resolved entry path inside the extension dir and a typed `ExportFormat`. Hosts that don't yet support running custom code can still surface the target as "unavailable" without erroring out.

Required permissions: `filesystem_write`.

### 4.5 AI tool (`type: "ai_tool"`)

Adds an AI tool the planner can dispatch to. The tool declares its `allowed_scopes`, `max_entities_modified`, and `grammar_key`, all of which mirror the built-in [`ToolSchema`](crates/aec_ai/src/tool_schema.rs) shape. The extension half of [`safety_validator`](crates/aec_ai/src/safety_validator.rs) is [`enforce_max_entities_modified`](crates/aec_ai/src/extension_tools.rs); a change set bigger than the declared cap is rejected before the diff is applied.

Required permissions: `ai_tools` (plus `geometry_read` / `geometry_write` to do anything useful).

### 4.6 Importer (`type: "importer"`)

Declares support for a vendor file format. The loader validates the manifest today; the runtime dispatch hook lands in the v1.1 import pipeline.

Required permissions: `filesystem_read`, `geometry_write`.

---

## 5. Permission model

Every permission a manifest needs MUST be listed in `permissions`. The loader rejects duplicates and unknown values; hosts then call [`PermissionEnforcer::check_permission`](crates/aec_core/src/extension_permissions.rs) before doing any privileged work.

| Permission        | What it grants                                                                                                |
| ----------------- | ------------------------------------------------------------------------------------------------------------- |
| `filesystem_read` | Read files inside the extension directory tree (asset payloads, template JSON, schedule JSON, etc.)           |
| `filesystem_write`| Write deliverable output from an `export_target` (and any helper files the target creates next to it).        |
| `network`         | Outbound network access. Currently no host implements this — manifests may declare it but it is unused.       |
| `geometry_read`   | Read the project graph (rooms, walls, entities) when building a schedule, exporting, or classifying.          |
| `geometry_write`  | Mutate the project graph. Asset packs MAY NOT request this — they only add catalogue rows.                    |
| `ai_tools`        | Register an AI tool the planner can dispatch to. Required by `ai_tool` extensions.                            |
| `audit_log`       | Write to the audit log (not just consume it). Reserved for v1.1 audit tooling.                                |

The mapping from `Operation` to required permission is in [`PermissionEnforcer::check_permission`](crates/aec_core/src/extension_permissions.rs). The enforcer is conservative: any check that doesn't match a declared permission returns `Denied{reason}` and the host refuses the operation.

---

## 6. Signature verification (Ed25519)

Extensions are signed with Ed25519. The signing payload is the manifest canonicalized through [`canonical_payload_bytes`](crates/aec_core/src/extensions.rs), which strips the `signature` field and re-serializes with sorted keys so signing is byte-stable.

Two verification entry points exist:

- [`verify_signature_against`](crates/aec_core/src/extension_permissions.rs) — checks the signature against a [`TrustStore`](crates/aec_core/src/extension_permissions.rs) of allowed public keys. Returns `UntrustedKey` when the manifest's public key is not in the store. **This is the production path.**
- [`verify_signature_self_consistent`](crates/aec_core/src/extension_permissions.rs) — checks only that the signature matches the embedded public key. Useful for development; does NOT prove the signer is trusted.

The loader's `LoadOptions` controls whether unsigned extensions are accepted. The default is conservative (`LoadOptions::default()` rejects unsigned), with `LoadOptions::allow_unsigned()` available as a dev mode. Unsigned extensions emit a user-visible warning in the Settings UI and are tagged `signed: false` on `LoadedExtension`.

### Signing a manifest

A complete signing flow looks like this (Rust, but the algorithm is identical in any language):

```rust
use aec_core::{canonical_payload_bytes, ExtensionManifest, ExtensionSignature};
use ed25519_dalek::{Signer, SigningKey};

let mut manifest: ExtensionManifest = serde_json::from_str(&raw)?;
manifest.signature = None;                                    // strip before signing
let payload = canonical_payload_bytes(&manifest)?;            // canonical bytes
let sk: SigningKey = /* load from secure storage */;
let sig = sk.sign(&payload);
manifest.signature = Some(ExtensionSignature {
    algorithm: "ed25519".into(),
    public_key_hex: hex::encode(sk.verifying_key().to_bytes()),
    signature_hex: hex::encode(sig.to_bytes()),
});
fs::write("manifest.json", serde_json::to_string_pretty(&manifest)?)?;
```

The hex format is lowercase, no `0x` prefix, exactly 64 hex chars for the public key and 128 hex chars for the signature.

### Distributing trusted keys

Trusted public keys live in `~/.config/aec-studio/trusted_keys.json` (Linux), `~/Library/Application Support/AEC Studio/trusted_keys.json` (macOS), or `%APPDATA%\AEC Studio\trusted_keys.json` (Windows). The file is a JSON map from key id → hex public key. The first-launch UI lets a user paste a key; an MDM-managed deployment can ship the file pre-populated.

---

## 7. Development guide

### 7.1 Create an asset pack

1. `mkdir -p extensions/acme.flagship-sofas/furniture`
2. Drop the meshes (`.glb`) under `furniture/`.
3. Compute the BLAKE3 of each mesh: `b3sum furniture/sofa.glb`.
4. Author `extensions/acme.flagship-sofas/manifest.json`:
   ```json
   {
     "id": "acme.flagship-sofas",
     "name": "ACME Flagship Sofas",
     "version": "1.0.0",
     "type": "asset_pack",
     "permissions": ["filesystem_read", "geometry_read"],
     "license": "AGPL-3.0",
     "description": "Three-seater and four-seater sofas.",
     "asset_pack": {
       "vendor": "ACME",
       "entries": [
         { "asset_id": "acme.sofa.3s",
           "name": "Three-seater Sofa",
           "kind": "furniture",
           "tags": ["living-room", "sofa"],
           "source_path": "furniture/sofa.glb",
           "blake3": "abc123..." }
       ]
     }
   }
   ```
5. Sign the manifest (§6) and ship.

### 7.2 Create a template

1. Author the `TemplateDefinition` JSON exactly like the shipped templates in `templates/`.
2. Drop it under `extensions/<id>/templates/<name>.json`.
3. Author the manifest with `type: "template"` and the `template.key` / `template.definition_path` fields pointing at the JSON.

### 7.3 Create a schedule

1. Pick a stable `schedule_id` (used as a key in the schedule registry).
2. Declare each column (`ScheduleColumnDef`) with its `key`, `header`, `value_type` (one of `string` / `integer` / `float` / `boolean`), and an optional `default`.
3. (Optional) declare formulas as free-form `expression` strings; the schedule host parses and evaluates them.

### 7.4 Create an export target

1. Author the entry script (e.g. a Lua / JSON descriptor) and place it under `entry_path` relative to the extension root.
2. Pick a `target_id`, set the `format` (one of `pdf` / `xlsx` / `zip` / `ifc` / `json` / `glb`), and a `default_extension`.
3. The Deliver UI lists the target alongside built-ins; running it invokes `entry_path` with the project context.

### 7.5 Create an AI tool

1. Pick a `tool_id` and a `grammar_key` from the bundled [`GrammarRegistry`](crates/aec_ai/src/grammars.rs) (the AI runtime loads the grammar before calling the model).
2. Declare `allowed_scopes` as a subset of `design` / `draft` / `bim` / `render` / `deliver`.
3. Set `max_entities_modified` to the **tightest** number that still lets the tool do its job. The safety validator rejects any diff bigger than this cap.

---

## 8. Security model

| Layer                       | Enforcement                                                                                          |
| --------------------------- | ---------------------------------------------------------------------------------------------------- |
| Permission gate             | `PermissionEnforcer` checks every privileged op against the manifest's declared `permissions`.       |
| Manifest validation         | `validate_manifest` rejects malformed manifests at install time, not at first use.                   |
| Signature verification      | `verify_signature_against` requires the public key to be in the `TrustStore`.                        |
| Asset integrity             | `install_asset_packs` recomputes BLAKE3 for every entry and rejects mismatches.                      |
| Safety envelope             | `enforce_max_entities_modified` caps how many entities an AI tool can change per dispatch.           |
| Audit log                   | Every extension-driven operation lands in the audit log via the host's standard write path.          |

Network access is disabled in v1 — no host implements the `network` permission, and the renderer's Electron sandbox blocks outbound HTTP from extensions. v1.1 adds an opt-in network broker for extensions that declare `network`.

---

## 9. References

- [`PROPOSAL.md`](PROPOSAL.md) §8 — high-level extension story
- [`ARCHITECTURE.md`](ARCHITECTURE.md) — where extensions sit in the crate graph
- [`PROGRESS.md`](PROGRESS.md) — phase exit criteria including the extension batch
- [`crates/aec_core/src/extensions.rs`](crates/aec_core/src/extensions.rs) — manifest schema + loader + registry
- [`crates/aec_core/src/extension_permissions.rs`](crates/aec_core/src/extension_permissions.rs) — validator + permission enforcer + Ed25519 verification
