//! N-API wrappers around [`BridgeService`]. Only compiled with the
//! `napi` feature (set when packaging the desktop app for Electron).

#![cfg(feature = "napi")]

use std::path::PathBuf;
use std::sync::Mutex;

use napi::bindgen_prelude::*;
use napi_derive::napi;

use crate::service::{BridgeConfig, BridgeService};

/// Process-wide bridge singleton. The Electron main process initialises
/// this once at startup; every other call goes through [`with_service`].
static SERVICE: Mutex<Option<BridgeService>> = Mutex::new(None);

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
    *SERVICE.lock().unwrap() = Some(svc);
    Ok(())
}

#[napi(object)]
pub struct ProjectSummaryJs {
    pub project_id: String,
    pub name: String,
    pub path: String,
    pub template_id: Option<String>,
}

impl From<crate::service::ProjectSummary> for ProjectSummaryJs {
    fn from(s: crate::service::ProjectSummary) -> Self {
        Self {
            project_id: s.project_id.to_string(),
            name: s.name,
            path: s.path,
            template_id: s.template_id,
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

#[napi(object)]
pub struct RuntimeStatusJs {
    pub tier: String,
    pub cpu_model: String,
    pub physical_cores: u32,
    pub total_ram_gb: f64,
    pub gpu_vendor: Option<String>,
    pub gpu_model: Option<String>,
    pub os: String,
}

#[napi]
pub fn runtime_status() -> Result<RuntimeStatusJs> {
    let mut guard = SERVICE
        .lock()
        .map_err(|e| Error::from_reason(e.to_string()))?;
    let svc = guard
        .as_mut()
        .ok_or_else(|| Error::from_reason("bridge not initialised"))?;
    let r = svc.runtime_status();
    Ok(RuntimeStatusJs {
        tier: r.tier.as_str().to_string(),
        cpu_model: r.cpu_model,
        physical_cores: r.physical_cores,
        total_ram_gb: r.total_ram_gb as f64,
        gpu_vendor: r.gpu_vendor,
        gpu_model: r.gpu_model,
        os: r.os,
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
