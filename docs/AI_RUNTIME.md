# AI runtime architecture

> **Scope.** This document covers the on-device AI runtime AEC Studio ships: text inference (the only AI capability live in v1). Image generation is a planned Phase 18 follow-on — see the **Future: text-to-image** section at the end for the current state of that work.

## TL;DR

| Aspect | What ships |
|---|---|
| Inference binary | [`llama-server`](https://github.com/kennguy3n/llama.cpp) (PrismML llama.cpp fork) |
| Models | Ternary-Bonsai 1.7B / 4B / 8B — **1.58-bit GGUF** (Q2_0) |
| macOS acceleration | llama.cpp **Metal** backend (`--n-gpu-layers 999`) |
| Windows acceleration | **CUDA** (NVIDIA) or **Vulkan** (AMD / Intel); `--no-mmap` on this platform only |
| Linux acceleration | **CUDA** (NVIDIA) or **Vulkan** (AMD / Intel) |
| CPU fallback | AVX2 / AVX-VNNI / AVX-512-VNNI (x86) or NEON (Apple Silicon Intel transition) |
| Process boundary | Out-of-process subprocess, HTTP loopback on `127.0.0.1` only |
| Python | **None.** The shipped runtime contains zero `*.py` files and no `python3` / `pip` / `mlx` / `conda` binaries. |

## No Python in the runtime

AEC Studio is a single-binary desktop app whose runtime AI dependencies are *only* the `llama-server` native binary plus a `*.gguf` model file. This is enforced at three layers:

1. **Source tree.** The repository contains no `workers/ai/` directory, no `requirements.txt` / `pyproject.toml`, no virtualenv. The two `.py` files in the tree — `scripts/generate_template_previews.py` and `packaging/generate_icons.py` — are build-time tooling that runs on the CI runner and emits PNGs; they are never invoked at app runtime and are excluded from the packaged installer.
2. **CI.** The `.github/workflows/ci.yml` job graph contains no `setup-python` step and no `pip install` step. The Phase 9 PR4 + PR5 changes removed the historical `python-workers` job entirely (Blender and IFC workers were both rewritten in native Rust under `crates/aec_render/` and `crates/aec_bim/`).
3. **Installers.** `packaging/{macos,windows,linux}/electron-builder.*.yml` package only `dist/`, `dist-electron/`, `package.json`, and `node_modules/`. No Python interpreter is bundled, no `mlx` / `mlx-lm` / `gemlite` / `HQQ` wheels are bundled, and the v1 release does **not** vendor a sidecar binary — users either install the PrismML `llama-server` from a release artifact or set `AEC_AI_PRISMML_BIN` to point at a custom build.

PrismML *also* publishes MLX-2bit checkpoints (`prism-ml/Ternary-Bonsai-*-mlx-2bit`) on Hugging Face. Those exist as **conversion sources** for the GGUF-Q2_0 files we actually load and ship metadata for — nothing in the AEC Studio runtime opens an MLX file, because MLX requires the Python `mlx` and `mlx-lm` packages.

## Models

| Tier | Hugging Face repo | Filename | Size on disk | BLAKE3 |
|---|---|---|---|---|
| Small | [`prism-ml/Ternary-Bonsai-1.7B-gguf`](https://huggingface.co/prism-ml/Ternary-Bonsai-1.7B-gguf) | `Ternary-Bonsai-1.7B-Q2_0.gguf` | ≈ 463 MB | `6634a3ae6c4a5b3e6bec28fd7abe701579c2739db3695df1eb8ced9c28e4fc9a` |
| Medium | [`prism-ml/Ternary-Bonsai-4B-gguf`](https://huggingface.co/prism-ml/Ternary-Bonsai-4B-gguf) | `Ternary-Bonsai-4B-Q2_0.gguf` | ≈ 1.07 GB | `89a7662c39f5c704e2ede224590e3840ab7d44213109f08164177a7b2d14a7f5` |
| Large | [`prism-ml/Ternary-Bonsai-8B-gguf`](https://huggingface.co/prism-ml/Ternary-Bonsai-8B-gguf) | `Ternary-Bonsai-8B-Q2_0.gguf` | ≈ 2.18 GB | `3c2a48b2e9da29274ec96770cbd27ed0dd2e14b57ba1ce20b1a1d68344738ddd` |

The BLAKE3 hashes are pinned at compile time in `crates/aec_ai/src/model_manager.rs::ModelTier::canonical_blake3_hex`. Downloading a file whose hash does not match is treated as a verification failure and the partial file is deleted; the integrity check is the only thing standing between the user and a tampered model, so it is hardcoded rather than fetched alongside the file.

### Storage locations

The default models directory is platform-specific:

| Platform | Path |
|---|---|
| macOS | `~/Library/Application Support/AEC Studio/models/` |
| Linux | `$XDG_DATA_HOME/aec-studio/models/` (fallback `~/.local/share/aec-studio/models/`) |
| Windows | `%LOCALAPPDATA%\AEC Studio\models\` |

Users can override these by setting `AEC_AI_MODELS_DIR` to an absolute path.

## Download flow

The bridge service downloads models via HTTPS directly from `huggingface.co`. The flow runs in `crates/aec_ai/src/model_download.rs`:

1. **Allow-list check.** Only `huggingface.co` and `cdn-lfs.huggingface.co` are accepted — the `Location` header on a 301 / 302 is re-parsed and re-checked against the same list, so a malicious mirror serving a redirect to `evil.example` is rejected before any bytes are read.
2. **`Range` resume.** If a `.partial` file from a previous attempt exists, the next request sends `Range: bytes=<offset>-`; the server's `Content-Range` is parsed and compared against the local offset to confirm the resume is genuine.
3. **Streaming write.** The body is streamed straight to the `.partial` file (no in-memory buffering) and a BLAKE3 hasher is updated incrementally.
4. **Verification.** On EOF the BLAKE3 result is compared against the compile-time-pinned hash for the tier. Mismatch → the `.partial` file is deleted, error propagated.
5. **Atomic rename.** On success the `.partial` file is renamed to the final name (`Ternary-Bonsai-1.7B-Q2_0.gguf`). The rename is atomic on every supported filesystem (NTFS, APFS, ext4, btrfs, ZFS).

### Privacy guarantees on download

* **User-Agent.** A fixed `AEC-Studio/<crate-version>` string is sent; no OS / browser / locale info.
* **No cookies.** The HTTP client never reads or stores cookies. `Set-Cookie` headers from the server are ignored.
* **No telemetry / analytics.** The download path has no callbacks to any non-Hugging Face domain. The redirect allow-list above is what enforces this.
* **TLS verification.** The default `rustls` certificate verifier is used; we do **not** disable certificate checking, do not pin certificates (which would force a release on every CA rotation), and do not accept self-signed certs.

## Sidecar lifecycle

Once a model is on disk, the bridge spawns `llama-server` via `crates/aec_ai/src/sidecar.rs::spawn`. The argv is built deterministically by `build_spawn_args(config, platform)`:

```
llama-server \
  --host 127.0.0.1 \
  --port <port> \
  --ctx-size <max_context_tokens> \
  --parallel <parallel> \
  --model <model_path> \
  --n-gpu-layers 999 \
  --flash-attn \
  [--no-mmap]   # Windows only
```

| Flag | Why |
|---|---|
| `--host 127.0.0.1` | Loopback only. The sidecar must never be reachable from anything but `localhost` — there is no auth on the inference endpoint. |
| `--n-gpu-layers 999` | "Offload everything you can." The PrismML fork falls back to CPU for layers that don't fit, so this is safe on every host. |
| `--flash-attn` | Flash-attention kernels. Ternary-Bonsai Q2_0 supports them on every backend the PrismML fork ships; \~30 % prefill speedup on Apple Silicon. |
| `--no-mmap` (Windows) | `MapViewOfFile`-based loading of multi-GB GGUF on Windows is unreliable on network shares / non-NTFS drives. Linux and macOS keep mmap on. |

Spawn / health / kill discipline lives in [`SidecarHandle`](../crates/aec_ai/src/sidecar.rs); dropping the handle kills the child via `kill()` so the loopback port and the multi-GiB mmap free immediately when the user switches tiers or closes the project.

## Switching tiers at runtime

`BridgeService::ai_set_active_tier(slug)`:

1. Updates `ModelManager::active_tier` (the descriptor lookup the renderer reads).
2. Calls `AiState::reload_with_config(new_config)`, which **kills the running `SidecarHandle`** (drops it) and replaces the in-process `SidecarRuntime` with a fresh `Idle` one carrying the new tier's `RuntimeConfig`.

The next `ai_plan` call cold-spawns `llama-server` against the new GGUF. The renderer does not need to issue a separate "restart sidecar" call.

## Where the wire format lives

The HTTP request / response shapes (`AiPlanRequest`, `AiPlanResponse`, JSON wire fields, error codes) live in `crates/aec_ai/src/transport.rs`. They are documented inline; see also `crates/aec_bridge/src/ai_endpoints.rs` for the bridge-side `ai:plan` IPC handler.

## Loopback-only, no outbound during inference

During `ai_plan` and any sidecar request, the only socket open from the AEC Studio process tree is the loopback connection to the sidecar. The sidecar itself never opens an outbound socket — `llama-server` does not phone home. The only egress traffic from the entire AI subsystem is the model download itself (Hugging Face HTTPS), which is gated behind the explicit "Download" button in Settings.

## Future: text-to-image

The Phase 18 spec called out a parallel image-generation runtime using PrismML's `bonsai-image-ternary-4B-gemlite-2bit`. As of this Phase 18 PR, **no image generation is shipped**: the published model is distributed in `gemlite` / `HQQ` formats that require the Python `gemlite`, `HQQ`, and `mlx` runtimes — none of which can be embedded under the no-Python constraint, and no native C/C++ loader for those formats exists today.

Two paths remain open for a future release:

1. Wait for PrismML to publish a native (e.g. C/C++ executable) inference server for `bonsai-image-ternary-4B`.
2. Switch to [`stable-diffusion.cpp`](https://github.com/leejet/stable-diffusion.cpp) with a different model (SDXL / FLUX-schnell in GGUF) — this would ship today under the same no-Python posture.

Neither has landed yet; the AI runtime as shipped in v1 is text-only.
