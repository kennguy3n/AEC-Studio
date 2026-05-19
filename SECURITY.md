# Security Policy

## Supported versions

| Version | Supported |
|---|---|
| Latest `main` | Yes |
| Older releases | Best effort |

AEC Studio is pre-1.0 software. Security fixes are applied to the latest `main` branch. Once stable releases begin, this table will track supported release branches.

---

## Reporting vulnerabilities

**Do not open a public GitHub issue for security vulnerabilities.**

### Preferred: GitHub Security Advisories

1. Go to the [Security Advisories page](https://github.com/kennguy3n/AEC-Studio/security/advisories).
2. Click **"Report a vulnerability"**.
3. Provide a detailed description of the vulnerability, steps to reproduce, and potential impact.

### Alternative: Email

Send a detailed report to **ken@uney.com** with:

- Description of the vulnerability.
- Steps to reproduce.
- Potential impact and severity assessment.
- Any suggested fixes or mitigations.

### Response timeline

| Action | Timeline |
|---|---|
| Acknowledgment | Within 48 hours |
| Initial assessment | Within 7 days |
| Fix or mitigation plan | Within 30 days |
| Public disclosure | After fix is released (coordinated) |

---

## Responsible disclosure policy

We follow coordinated disclosure:

1. **Report privately** using the methods above.
2. **We acknowledge** your report within 48 hours.
3. **We assess** the severity and develop a fix.
4. **We release** the fix and credit you (unless you prefer anonymity).
5. **You may disclose** publicly after the fix is released.

We ask that you:

- Give us reasonable time to address the issue before public disclosure.
- Do not exploit the vulnerability beyond what is necessary to demonstrate it.
- Do not access or modify other users' data.

---

## Security design principles

AEC Studio is built around these security principles:

### Local-first data sovereignty

All project data — geometry, materials, IFC models, renders, schedules, AI action logs — is stored locally on the user's machine by default. There is no cloud backend, no telemetry, and no analytics. Nothing leaves the device unless the user explicitly exports a file, runs an opt-in sync, or publishes to KChat (which is itself opt-in and confined to artifact cards).

### Encrypted local storage

Every project's `project.sqlite` is stored in **SQLCipher** with AES-256 page-level encryption and per-project keys. The asset database, audit log, and AI action log use the same encryption substrate.

Content hashing throughout the package (geometry blobs, command journal, audit chain) uses **BLAKE3** for integrity and dedup. Per-scope DEKs follow the same cryptographic-forgetting pattern as [kennguy3n/knowledge](https://github.com/kennguy3n/knowledge): destroying a scope key permanently invalidates the data in that scope.

### Safe renderer boundary

The Electron renderer (React UI) operates in a sandboxed context with:

- **`contextIsolation: true`** — renderer JavaScript cannot access Node.js APIs.
- **`nodeIntegration: false`** — no `require()` or `process` in the renderer.
- **Typed IPC only** — all communication between renderer and main process goes through a typed, validated IPC bridge exposed via `contextBridge`.
- **No direct file access** — the renderer cannot read files, launch processes, or interact with the encrypted DB directly.
- **No direct worker access** — the renderer cannot launch or message the Blender, IFC, or AI workers; only the main process can.
- **Content Security Policy** — strict CSP headers prevent inline scripts, `eval`, and unauthorized resource loading.

### Strict process separation

| Process | Access |
|---|---|
| Renderer (React) | UI only — no file system, no tokens, no database, no workers |
| Main (Electron) | IPC routing, window management, OS file picker, worker supervision |
| Rust core (N-API) | Project graph, geometry, asset DB, render queue, export, audit |
| Blender worker | Rendering only, runs as a separate Blender process with stdin/stdout JSON IPC |
| IFC worker | BIM/IFC parsing only, runs as a separate process wrapping IfcOpenShell |
| AI sidecar (llama-server) | Model inference, bound to localhost loopback only |

### Worker process isolation

Blender, IFC, and AI workers run as **separate processes**. They communicate with the Rust core via either local pipes (stdin/stdout JSON-line IPC) or a loopback HTTP socket (for the AI sidecar). They never share address space with AEC Studio's main binary. A crash in a worker never crashes the main app — the supervisor restarts the worker and surfaces the failure in the UI.

### AI safety

- AI cannot mutate geometry, materials, schedules, or sheets silently. Every action surfaces as a previewable diff that the user must accept.
- Every AI request is constrained by a registered tool schema (typed JSON Schema) and grammar-constrained decoding (GBNF). The planner cannot emit operations outside the schema.
- A safety validator enforces per-tool scope (`design` / `draft` / `bim` / `render` / `deliver`) and bounded changes (`max_entities_modified`).
- AI cannot trigger export, sync, network, or any side-effectful operation directly.
- AI compute mode (model tier, device tier, sidecar state) is visible in the status bar at all times.

### Project file encryption

A `.aecstudio` project package contains an encrypted SQLCipher database and a content-addressed blob store. Per-project keys are derived from a user-level master key (stored in the OS keychain) and a per-project nonce. Sharing a project means sharing both the package and the key — there is no implicit cloud sharing.

### Audit trail

All security-relevant actions are logged to an append-only audit trail:

- Project create / open / save / export.
- Command engine mutations (with command hash, actor, scope).
- AI action proposals, acceptances, and rejections (with diff hash, tool, model, sidecar id).
- Extension loads (with extension id and permissions snapshot).
- Worker start / stop / crash events.
- Export operations (target, sha256, size, success/failure).

The audit log is hash-chained per entry and can be exported alongside a project.

---

## Scope

### In scope for security reports

- Renderer sandbox escapes (accessing Node.js APIs, file system, workers, or tokens from the renderer).
- IPC message injection or spoofing.
- Encrypted storage bypass (reading SQLCipher data without the key).
- Arbitrary code execution through crafted files (e.g., malicious DXF, IFC, PDF, glTF, FBX, asset pack).
- AI sidecar escaping loopback (accepting connections from outside localhost) or exfiltrating data.
- Worker process escapes (Blender, IFC, AI) that affect AEC Studio's security boundary.
- Audit log tampering (breaking the hash chain without detection).
- Extension permissions bypass (extension performing operations outside its declared permissions).

### Out of scope

- Vulnerabilities in upstream dependencies (report these to the upstream project).
- Issues requiring physical access to an unlocked machine (this is a desktop app; physical access implies full access).
- Social engineering attacks.
- Denial of service against the local application (crashing your own app is not a security issue).
- Issues in [llama.cpp](https://github.com/kennguy3n/llama.cpp), [Blender](https://www.blender.org/), or [IfcOpenShell](https://github.com/kennguy3n/IfcOpenShell) that do not affect AEC Studio's security boundary — please report those upstream.

---

## Dependencies

AEC Studio uses well-maintained dependencies with known security properties:

| Dependency | Purpose | Security note |
|---|---|---|
| SQLCipher (via rusqlite) | Encrypted local storage | AES-256 page-level encryption |
| BLAKE3 | Content hashing | Cryptographic hash function |
| Electron | Desktop shell | Chromium sandbox + process isolation, kept on the latest stable |
| napi-rs | Rust ↔ Node.js bridge | Type-safe, no serialization vulnerabilities |
| wgpu | Cross-platform GPU | Safe Rust API over Vulkan / Metal / D3D12 / OpenGL |
| llama.cpp / PrismML | Local AI inference | Runs as a sidecar bound to loopback |
| IfcOpenShell | BIM/IFC parsing | Runs as an isolated worker process |
| Blender | Rendering | Runs as an isolated worker process; never linked |

We monitor dependencies for known vulnerabilities and update promptly. Dependabot or an equivalent watcher runs on the repo.
