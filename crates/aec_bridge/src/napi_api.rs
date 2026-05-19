//! N-API wrappers around [`BridgeService`]. Only compiled with the
//! `napi` feature (set when packaging the desktop app for Electron).
//!
//! Every JS-facing struct in this file is intentionally aligned, field
//! for field, with the corresponding TypeScript interface in
//! `apps/desktop/electron/bridge.ts`. The bridge layer in TypeScript
//! casts the N-API return values to those interfaces with no
//! transformation, so any drift between this file and the TS interfaces
//! is a runtime bug surfaced as `undefined` on the renderer side.

#![cfg(feature = "napi")]

use std::path::PathBuf;
use std::sync::Mutex;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::service::{BridgeConfig, BridgeService};

/// Process-wide bridge singleton. The Electron main process initialises
/// this once at startup; every other call goes through [`with_service`].
static SERVICE: Mutex<Option<BridgeService>> = Mutex::new(None);

/// Run `f` with mutable access to the bridge singleton, converting all
/// lock poisoning and "not initialised" errors into typed N-API errors so
/// the renderer can recover gracefully instead of crashing the host.
fn with_service<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&mut BridgeService) -> std::result::Result<R, crate::service::BridgeServiceError>,
{
    let mut guard = SERVICE
        .lock()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    let svc = guard
        .as_mut()
        .ok_or_else(|| Error::from_reason("bridge not initialised"))?;
    f(svc).map_err(|e| Error::from_reason(e.to_string()))
}

/// Same as [`with_service`] but for read-only callers that just need a
/// shared reference. Sharing the lock/unwrap pattern in one place avoids
/// drift between `with_service` and ad-hoc lock sites (the previous
/// `runtime_status` implementation had its own copy, which had to be
/// updated in lockstep every time poisoning semantics changed).
fn with_service_ref<F, R>(f: F) -> Result<R>
where
    F: FnOnce(&BridgeService) -> R,
{
    let guard = SERVICE
        .lock()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    let svc = guard
        .as_ref()
        .ok_or_else(|| Error::from_reason("bridge not initialised"))?;
    Ok(f(svc))
}

#[napi(object)]
pub struct InitOptions {
    pub state_dir: String,
    pub projects_dir: String,
    pub templates_dir: String,
    pub max_recents: u32,
    /// 32-byte master key (hex-encoded).
    pub master_key_hex: String,
}

#[napi]
pub fn bridge_init(opts: InitOptions) -> Result<()> {
    let bytes = hex_decode(&opts.master_key_hex)
        .ok_or_else(|| Error::from_reason("master_key_hex must be 64 hex chars"))?;
    if bytes.len() != 32 {
        return Err(Error::from_reason("master_key_hex must decode to 32 bytes"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&bytes);
    let cfg = BridgeConfig {
        state_dir: PathBuf::from(opts.state_dir),
        projects_dir: PathBuf::from(opts.projects_dir),
        templates_dir: PathBuf::from(opts.templates_dir),
        max_recents: opts.max_recents as usize,
    };
    let svc = BridgeService::new(cfg, key).map_err(|e| Error::from_reason(e.to_string()))?;
    // Same poison-tolerant pattern as `with_service` so a previously-
    // panicked init can't bring down the Electron host with a cryptic
    // `PoisonError` — surface the failure as a typed N-API error.
    let mut guard = SERVICE
        .lock()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    *guard = Some(svc);
    Ok(())
}

/// JS-facing project summary. Field names are chosen so napi-rs's
/// snake_case → camelCase conversion produces exactly the names the
/// TypeScript `ProjectSummary` interface expects (`projectId`, `name`,
/// `path`, `templateKey`, `modifiedAt`).
///
/// Drift between this struct and `apps/desktop/electron/bridge.ts`
/// `ProjectSummary` is enforced informally by tests on both sides — keep
/// them aligned when adding fields.
#[napi(object)]
pub struct ProjectSummaryJs {
    pub project_id: String,
    pub name: String,
    pub path: String,
    /// Renamed from `template_id` so napi-rs exposes the JS field as
    /// `templateKey`, matching the TypeScript interface.
    pub template_key: Option<String>,
    /// ISO-8601 timestamp of the last manifest write (the recents store
    /// uses the last-opened moment, which is the user-perceived
    /// freshness on the Home dashboard). Always populated.
    pub modified_at: String,
}

impl From<crate::service::ProjectSummary> for ProjectSummaryJs {
    fn from(s: crate::service::ProjectSummary) -> Self {
        Self {
            project_id: s.project_id.to_string(),
            name: s.name,
            path: s.path,
            template_key: s.template_id,
            modified_at: s.updated_at.to_rfc3339(),
        }
    }
}

#[napi]
pub fn project_create_from_template(
    template_key: String,
    project_name: String,
) -> Result<ProjectSummaryJs> {
    with_service(|svc| svc.project_create_from_template(&template_key, &project_name))
        .map(Into::into)
}

#[napi]
pub fn project_open(path: String) -> Result<ProjectSummaryJs> {
    with_service(|svc| svc.project_open(&path)).map(Into::into)
}

#[napi]
pub fn project_save(path: String) -> Result<ProjectSummaryJs> {
    with_service(|svc| svc.project_save(&path)).map(Into::into)
}

#[napi]
pub fn project_list_recents() -> Result<Vec<ProjectSummaryJs>> {
    with_service(|svc| svc.project_list_recents()).map(|v| v.into_iter().map(Into::into).collect())
}

/// JS-facing CPU descriptor. Mirrors `RuntimeStatus["cpu"]` in
/// `apps/desktop/electron/bridge.ts`.
#[napi(object)]
pub struct CpuProfileJs {
    pub model: String,
    pub physical_cores: u32,
    pub logical_cores: u32,
}

/// JS-facing GPU descriptor. Mirrors the (non-null branch of)
/// `RuntimeStatus["gpu"]` in `apps/desktop/electron/bridge.ts`.
#[napi(object)]
pub struct GpuProfileJs {
    pub vendor: String,
    pub model: String,
    pub vram_mb: u32,
}

/// JS-facing hardware status. Field names + tier casing are deliberately
/// chosen to match the TypeScript `RuntimeStatus` interface so the
/// renderer can use the value as-is. In particular:
///
/// * `tier` is PascalCase (`"Low"` / `"Medium"` / `"High"` / `"Pro"`)
///   because the `is-tier-<Tier>` CSS classes in
///   `renderer/src/styles/components.css` use PascalCase.
/// * `cpu` and `gpu` are nested objects (not flat fields) because the
///   React components destructure them that way.
/// * `gpu` is nullable; outside Electron there is no wgpu adapter to
///   describe, and a misleading "software" placeholder would route
///   tier-dependent UI through the wrong code paths.
/// * RAM is reported in MiB; the TS `formatGb` helper renders it.
#[napi(object)]
pub struct RuntimeStatusJs {
    pub tier: String,
    pub cpu: CpuProfileJs,
    pub ram_total_mb: u32,
    pub ram_available_mb: u32,
    pub gpu: Option<GpuProfileJs>,
    pub os: String,
}

#[napi]
pub fn runtime_status() -> Result<RuntimeStatusJs> {
    with_service_ref(|svc| {
        let r = svc.runtime_status();
        RuntimeStatusJs {
            tier: r.tier.display().to_string(),
            cpu: CpuProfileJs {
                model: r.cpu.model.clone(),
                physical_cores: r.cpu.physical_cores,
                logical_cores: r.cpu.logical_cores,
            },
            // Cap at u32::MAX (~4 PiB). Any real host below that.
            ram_total_mb: r.total_ram_mb.min(u32::MAX as u64) as u32,
            ram_available_mb: r.available_ram_mb.min(u32::MAX as u64) as u32,
            gpu: r.gpu.as_ref().map(|g| GpuProfileJs {
                vendor: g.vendor.clone(),
                model: g.model.clone(),
                vram_mb: g.vram_mb,
            }),
            os: r.os.clone(),
        }
    })
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16).ok()?;
        out.push(byte);
    }
    Some(out)
}
