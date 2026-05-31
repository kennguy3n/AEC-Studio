//! Phase 18 Group C — text-to-image infrastructure.
//!
//! Mirrors the text-side `crate::sidecar` / `crate::transport` /
//! `crate::model_manager` / `crate::runtime` triplet, but for a separate
//! image-gen sidecar process. The two runtimes are deliberately
//! independent — each owns its own port, model files, child process, and
//! lifecycle — because:
//!
//!   * **GPU contention.** Loading a text model and an image-diffusion
//!     model into the same llama.cpp process is not supported, and even
//!     if it were, peak VRAM during diffusion sampling would starve
//!     concurrent text completions. Separate processes let the governor
//!     evict one to make room for the other.
//!   * **Binary swap.** The current image-gen backend is
//!     `stable-diffusion.cpp` (leejet's pure-C++ port; AGPL-compatible,
//!     no Python). PrismML's bonsai-image-ternary-4B-gemlite-2bit is a
//!     CUDA + gemlite + HQQ stack today and cannot be loaded by any
//!     native binary. The bridge surface here is deliberately
//!     model-agnostic — when a native bonsai-image server ships, swap
//!     the binary via [`IMAGE_GEN_BIN_ENV`] and the default model
//!     descriptor via [`model_manager::DEFAULT_IMAGE_GEN_MODEL`], and
//!     nothing else in the bridge / renderer / governor needs to
//!     change.
//!   * **Idle unload.** Image models are large (1–5 GiB on disk, 2–10
//!     GiB resident). The runtime's idle-unload timer (default 120 s)
//!     is longer than text's (60 s) but still kicks in eagerly so the
//!     user does not pay the resident cost when image-gen is unused.

pub mod model_manager;
pub mod runtime;
pub mod sidecar;
pub mod transport;

pub use model_manager::{
    ImageGenModelDescriptor, ImageGenModelManager, ImageGenModelManagerError,
    DEFAULT_IMAGE_GEN_MODEL,
};
pub use runtime::{ImageGenRuntime, ImageGenRuntimeError, ImageGenRuntimeState};
pub use sidecar::{
    build_image_gen_spawn_args, image_gen_bin, spawn_with_retry as image_gen_spawn_with_retry,
    ImageGenConfig, ImageGenHandle, ImageGenRestartPolicy, ImageGenSpawnError,
    DEFAULT_IMAGE_GEN_BIN, DEFAULT_IMAGE_GEN_PORT, IMAGE_GEN_BIN_ENV,
};
pub use transport::{
    ImageGenRequest, ImageGenResponse, ImageGenTransport, ImageGenTransportError,
    DEFAULT_IMAGE_GEN_REQUEST_TIMEOUT,
};
