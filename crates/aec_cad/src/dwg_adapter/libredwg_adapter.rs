//! LibreDWG adapter (`dwgread` / `dwgwrite`).
//!
//! Uses the LibreDWG `dwgread` tool to produce DXF from DWG and
//! `dwgwrite` to go the other way. Both are simple stdin/stdout-aware
//! tools in modern releases, but we use the canonical positional-arg
//! interface for compatibility.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::dwg_adapter::adapter::{DwgConverter, DwgError, DwgResult, DwgVersion};

#[derive(Debug, Clone)]
pub struct LibreDwgAdapter {
    pub dwgread: PathBuf,
    pub dwgwrite: PathBuf,
}

impl LibreDwgAdapter {
    pub fn new(dwgread: impl Into<PathBuf>, dwgwrite: impl Into<PathBuf>) -> Self {
        Self {
            dwgread: dwgread.into(),
            dwgwrite: dwgwrite.into(),
        }
    }
}

fn version_arg(v: DwgVersion) -> &'static str {
    match v {
        DwgVersion::R12 => "r12",
        DwgVersion::R14 => "r14",
        DwgVersion::R2000 => "r2000",
        DwgVersion::R2004 => "r2004",
        DwgVersion::R2007 => "r2007",
        DwgVersion::R2010 => "r2010",
        DwgVersion::R2013 => "r2013",
        DwgVersion::R2018 => "r2018",
    }
}

impl DwgConverter for LibreDwgAdapter {
    fn name(&self) -> &'static str {
        "LibreDWG"
    }

    fn check(&self) -> DwgResult<()> {
        if !self.dwgread.exists() {
            return Err(DwgError::BinaryMissing(self.dwgread.clone()));
        }
        if !self.dwgwrite.exists() {
            return Err(DwgError::BinaryMissing(self.dwgwrite.clone()));
        }
        Ok(())
    }

    fn dwg_to_dxf(&self, input_dwg: &Path, output_dxf: &Path) -> DwgResult<()> {
        self.check()?;
        if !input_dwg.exists() {
            return Err(DwgError::InputMissing(input_dwg.to_path_buf()));
        }
        let output = Command::new(&self.dwgread)
            .arg("--as=DXF")
            .arg("-o")
            .arg(output_dxf)
            .arg(input_dwg)
            .output()
            .map_err(|e| DwgError::SpawnFailed(e.to_string()))?;
        if !output.status.success() {
            return Err(DwgError::ConverterFailed {
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        if !output_dxf.exists() {
            return Err(DwgError::OutputMissing(output_dxf.to_path_buf()));
        }
        Ok(())
    }

    fn dxf_to_dwg(
        &self,
        input_dxf: &Path,
        output_dwg: &Path,
        version: DwgVersion,
    ) -> DwgResult<()> {
        self.check()?;
        if !input_dxf.exists() {
            return Err(DwgError::InputMissing(input_dxf.to_path_buf()));
        }
        let output = Command::new(&self.dwgwrite)
            .arg(format!("--as={}", version_arg(version)))
            .arg("-o")
            .arg(output_dwg)
            .arg(input_dxf)
            .output()
            .map_err(|e| DwgError::SpawnFailed(e.to_string()))?;
        if !output.status.success() {
            return Err(DwgError::ConverterFailed {
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        if !output_dwg.exists() {
            return Err(DwgError::OutputMissing(output_dwg.to_path_buf()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_binaries_check_fails() {
        let a = LibreDwgAdapter::new("/nonexistent/dwgread", "/nonexistent/dwgwrite");
        assert!(a.check().is_err());
    }
}
