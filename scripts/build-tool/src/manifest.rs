use std::{fs, path::Path};

use serde::Serialize;

use super::{BuildResult, path_error};

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct BuildManifest<'a> {
    pub schema_version: u32,
    pub created_at_unix_seconds: u64,
    pub configuration: &'a str,
    pub platform: &'a str,
    pub rust_target: &'a str,
    pub pe_machine_type: String,
    pub windows_kit_version: &'a str,
    pub driver_test_certificate_thumbprint: &'a str,
    pub executable_certificate_thumbprint: &'a str,
    pub files: Vec<ManifestFile<'a>>,
}

impl BuildManifest<'_> {
    pub(super) fn write(&self, path: &Path) -> BuildResult<()> {
        let mut manifest = serde_json::to_string_pretty(self).map_err(|error| {
            format!(
                "failed to serialize build manifest '{}': {error}",
                path.display()
            )
        })?;
        manifest.push('\n');
        fs::write(path, manifest).map_err(|error| path_error("write build manifest", path, error))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct ManifestFile<'a> {
    pub role: &'a str,
    pub path: String,
    pub length: u64,
    #[serde(rename = "SHA256")]
    pub sha256: String,
}
