//! ODA File Converter adapter.
//!
//! ODA's converter takes input and output directories plus version /
//! file-format strings; we wrap that in a single-file API.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::dwg_adapter::adapter::{DwgConverter, DwgError, DwgResult, DwgVersion};

#[derive(Debug, Clone)]
pub struct OdaAdapter {
    pub binary: PathBuf,
    pub output_dwg_version: DwgVersion,
}

impl OdaAdapter {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            output_dwg_version: DwgVersion::R2018,
        }
    }
}

fn version_arg(v: DwgVersion) -> &'static str {
    match v {
        DwgVersion::R12 => "ACAD12",
        DwgVersion::R14 => "ACAD14",
        DwgVersion::R2000 => "ACAD2000",
        DwgVersion::R2004 => "ACAD2004",
        DwgVersion::R2007 => "ACAD2007",
        DwgVersion::R2010 => "ACAD2010",
        DwgVersion::R2013 => "ACAD2013",
        DwgVersion::R2018 => "ACAD2018",
    }
}

impl DwgConverter for OdaAdapter {
    fn name(&self) -> &'static str {
        "ODA File Converter"
    }

    fn check(&self) -> DwgResult<()> {
        if !self.binary.exists() {
            return Err(DwgError::BinaryMissing(self.binary.clone()));
        }
        Ok(())
    }

    fn dwg_to_dxf(&self, input_dwg: &Path, output_dxf: &Path) -> DwgResult<()> {
        self.check()?;
        if !input_dwg.exists() {
            return Err(DwgError::InputMissing(input_dwg.to_path_buf()));
        }
        // ODA File Converter signature:
        //   ODAFileConverter <inputFolder> <outputFolder> <outputVersion>
        //     <outputFileType:DWG=0,DXF=1,…> <recurse:0|1> <auditFlag:0|1>
        //     [filter pattern]
        let input_dir = input_dwg.parent().unwrap_or(Path::new("."));
        let output_dir = output_dxf.parent().unwrap_or(Path::new("."));
        let mut cmd = Command::new(&self.binary);
        cmd.arg(input_dir);
        cmd.arg(output_dir);
        cmd.arg(version_arg(self.output_dwg_version));
        cmd.arg("1"); // DXF output
        cmd.arg("0"); // no recursion
        cmd.arg("1"); // audit
        if let Some(file_name) = input_dwg.file_name() {
            cmd.arg(file_name);
        }
        let output = cmd
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
        let input_dir = input_dxf.parent().unwrap_or(Path::new("."));
        let output_dir = output_dwg.parent().unwrap_or(Path::new("."));
        let mut cmd = Command::new(&self.binary);
        cmd.arg(input_dir);
        cmd.arg(output_dir);
        cmd.arg(version_arg(version));
        cmd.arg("0"); // DWG output
        cmd.arg("0");
        cmd.arg("1");
        if let Some(file_name) = input_dxf.file_name() {
            cmd.arg(file_name);
        }
        let output = cmd
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
    use std::path::PathBuf;

    #[test]
    fn missing_binary_check_fails() {
        let adapter = OdaAdapter::new(PathBuf::from("/nonexistent/odaconv"));
        assert!(adapter.check().is_err());
    }

    #[test]
    fn missing_input_returns_error() {
        let adapter = OdaAdapter::new(PathBuf::from("/nonexistent/odaconv"));
        let r = adapter.dwg_to_dxf(Path::new("/tmp/nonexistent.dwg"), Path::new("/tmp/out.dxf"));
        assert!(r.is_err());
    }
}
