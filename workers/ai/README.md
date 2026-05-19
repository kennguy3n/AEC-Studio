# PrismML local AI sidecar

The AEC Studio AI pipeline never leaves the user's machine. We use a
local llama.cpp server (referred to as **PrismML** throughout the codebase
to keep naming neutral between models) that the Rust `aec_ai` crate
launches as a child process when the renderer asks for an AI action.

## Layout

| File | Purpose |
|------|---------|
| `config.json` | Default sidecar configuration (model, port, context, idle timeout, tool whitelist). |
| `README.md` *(this file)* | Local setup instructions. |

The runtime lifecycle (start, health check, idle unload, restart) lives
in `crates/aec_ai/src/runtime.rs`. The tool schemas, GBNF grammars,
safety validator, and diff engine all live in the same crate.

## Local setup

1. Build or install the `llama-server` binary from the bundled
   `kennguy3n/llama.cpp` fork at the version pinned in
   `docs/LICENSE_ARCHITECTURE.md` (MIT, bundling-friendly).
2. Drop a `.gguf` model into the directory pointed to by
   `models_dir` in `config.json`. The Phase 1 default is
   `prismml-7b-q4_k_m.gguf`; larger tiers (13B, 34B) live under the
   same naming convention.
3. Edit `config.json` if you need to override:
   * `server.port` — pick a free port if 13579 is taken.
   * `server.n_gpu_layers` — set > 0 if a CUDA/Metal/Vulkan device
     should accelerate inference.
   * `server.parallel` — number of concurrent slots (Phase 1 default
     of 2 covers a foreground AI call plus an opportunistic
     background validation).

## Boot

The Rust runtime spawns the sidecar with arguments equivalent to:

```bash
llama-server \
  --host 127.0.0.1 --port 13579 \
  --ctx-size 4096 \
  --parallel 2 \
  --n-gpu-layers 0 \
  --model "${HOME}/.aec/models/prismml-7b-q4_k_m.gguf"
```

The server stays warm for 60 s of inactivity, then unloads. The next
request triggers a re-start; the queue blocks at most ~200 ms while the
model maps in.

## Tool calls and grammars

Each tool registered in `config.json` corresponds to a `.gbnf` file in
`crates/aec_ai/src/grammars`. The Rust planner ships the grammar with
every request so the model can only emit tool-call JSON that matches the
expected schema. The safety validator then re-checks the produced JSON
against the tool's caps (`max_entities_modified`, allowed scope, etc.)
before any diff is built.

## Testing without a real model

The `aec_ai` crate's unit tests do **not** spawn `llama-server`; they
exercise the runtime state machine, the safety validator, the grammar
loader, and the diff engine in-process with synthetic tool-call JSON.
This keeps CI deterministic and fast.

For end-to-end verification with a real model, set `AEC_AI_PRISMML_BIN`
and `AEC_AI_PRISMML_MODEL`; the integration test under
`crates/aec_ai/tests/runtime.rs` will pick them up and run the warm/
serve/unload cycle against the actual binary.

## Security posture

* `deny_network` — the sidecar listens only on loopback and refuses
  outbound connections initiated from tool calls.
* `deny_filesystem_outside_project` — file paths in tool inputs are
  validated against the active project root before any read/write.
* Tool whitelist — only the entries declared in `config.json[tools]`
  may be invoked. Out-of-scope tools are rejected by the safety
  validator and logged to the audit chain.
