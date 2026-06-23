use anyhow::{format_err, Context, Result};
use std::io::Read;
use strum::IntoEnumIterator;

mod pipfile;
mod resolver;

#[derive(Clone, Debug)]
pub struct PyExtension {
    name_: String,
    registry_host_names_: Vec<String>,
    registry_human_url_template_: String,
}

impl thirdpass_core::extension::FromLib for PyExtension {
    fn new() -> Self {
        Self {
            name_: "py".to_string(),
            registry_host_names_: vec!["pypi.org".to_owned()],
            registry_human_url_template_:
                "https://pypi.org/pypi/{{package_name}}/{{package_version}}/".to_string(),
        }
    }
}

impl thirdpass_core::extension::Extension for PyExtension {
    fn name(&self) -> String {
        self.name_.clone()
    }

    fn registries(&self) -> Vec<String> {
        self.registry_host_names_.clone()
    }

    fn review_target_policy(&self) -> thirdpass_core::extension::ReviewTargetPolicy {
        thirdpass_core::extension::ReviewTargetPolicy::default()
    }

    /// Returns a list of dependencies for the given package.
    ///
    /// Returns one package dependencies structure per registry.
    fn identify_package_dependencies(
        &self,
        package_name: &str,
        package_version: &Option<&str>,
        extension_args: &[String],
    ) -> Result<Vec<thirdpass_core::extension::PackageDependencies>> {
        resolver::identify_package_dependencies(package_name, package_version, extension_args)
    }

    fn identify_file_defined_dependencies(
        &self,
        working_directory: &std::path::Path,
        _extension_args: &[String],
    ) -> Result<Vec<thirdpass_core::extension::FileDefinedDependencies>> {
        // Identify all dependency definition files.
        let dependency_files = match identify_dependency_files(working_directory) {
            Some(v) => v,
            None => return Ok(Vec::new()),
        };

        // Read all dependencies definitions files.
        let mut all_dependency_specs = Vec::new();
        for dependency_file in dependency_files {
            // TODO: Add support for parsing all definition file types.
            let (dependencies, registry_host_name) = match dependency_file.r#type {
                DependencyFileType::PipfileLock => (
                    pipfile::get_dependencies(&dependency_file.path)?,
                    pipfile::get_registry_host_name(),
                ),
            };
            all_dependency_specs.push(thirdpass_core::extension::FileDefinedDependencies {
                path: dependency_file.path,
                registry_host_name,
                dependencies: dependencies.into_iter().collect(),
            });
        }

        Ok(all_dependency_specs)
    }

    fn registries_package_metadata(
        &self,
        package_name: &str,
        package_version: &Option<&str>,
    ) -> Result<Vec<thirdpass_core::extension::RegistryPackageMetadata>> {
        let package_version = match package_version {
            Some(v) => Some(v.to_string()),
            None => get_latest_version(package_name)?,
        }
        .ok_or(format_err!("Failed to find package version."))?;

        // Currently, only one registry is supported. Therefore simply select first.
        let registry_host_name = self
            .registries()
            .first()
            .ok_or(format_err!(
                "Code error: vector of registry host names is empty."
            ))?
            .clone();

        let entry_json = get_registry_entry_json(package_name)?;
        let artifact_url = get_archive_url(&entry_json, &package_version)?;
        let human_url = get_registry_human_url(self, package_name, &package_version)?;

        Ok(vec![thirdpass_core::extension::RegistryPackageMetadata {
            registry_host_name,
            human_url: human_url.to_string(),
            artifact_url: artifact_url.to_string(),
            is_primary: true,
            package_version: package_version.to_string(),
        }])
    }
}

/// Given package name, return latest version.
fn get_latest_version(package_name: &str) -> Result<Option<String>> {
    let json = get_registry_entry_json(package_name)?;
    let releases = json["releases"]
        .as_object()
        .ok_or(format_err!("Failed to find releases JSON section."))?;
    let mut versions: Vec<semver::Version> = releases
        .keys()
        .filter(|v| v.chars().all(|c| c.is_numeric() || c == '.'))
        .filter_map(|v| semver::Version::parse(v).ok())
        .collect();
    versions.sort();

    let latest_version = versions.last().map(|v| v.to_string());
    Ok(latest_version)
}

fn get_registry_human_url(
    extension: &PyExtension,
    package_name: &str,
    package_version: &str,
) -> Result<url::Url> {
    // Example return value: https://pypi.org/pypi/numpy/1.18.5/
    let handlebars_registry = handlebars::Handlebars::new();
    let human_url = handlebars_registry.render_template(
        &extension.registry_human_url_template_,
        &maplit::btreemap! {
            "package_name" => package_name,
            "package_version" => package_version,
        },
    )?;
    Ok(url::Url::parse(human_url.as_str())?)
}

fn get_registry_entry_json(package_name: &str) -> Result<serde_json::Value> {
    let handlebars_registry = handlebars::Handlebars::new();
    let url = handlebars_registry.render_template(
        "https://pypi.org/pypi/{{package_name}}/json",
        &maplit::btreemap! {
            "package_name" => package_name,
        },
    )?;
    let mut result = reqwest::blocking::get(&url.to_string())?;
    let mut body = String::new();
    result.read_to_string(&mut body)?;

    serde_json::from_str(&body).context(format!("JSON was not well-formatted:\n{}", body))
}

fn get_archive_url(
    registry_entry_json: &serde_json::Value,
    package_version: &str,
) -> Result<url::Url> {
    let releases_section = registry_entry_json
        .get("releases")
        .ok_or(format_err!("Failed to find releases JSON section."))?;
    let release_entry = releases_section.get(package_version).ok_or(format_err!(
        "Package version not found in registry releases: {}",
        package_version
    ))?;
    let releases = release_entry.as_array().ok_or(format_err!(
        "Registry releases entry for version {} is not an array.",
        package_version
    ))?;
    if releases.is_empty() {
        return Err(format_err!(
            "No release artifacts found for version {}.",
            package_version
        ));
    }
    for release in releases {
        let python_version = release["python_version"]
            .as_str()
            .ok_or(format_err!("Failed to parse package version."))?;
        if python_version == "source" {
            return Ok(url::Url::parse(
                release["url"]
                    .as_str()
                    .ok_or(format_err!("Failed to parse package archive URL."))?,
            )?);
        }
    }
    Err(format_err!("Failed to identify package archive URL."))
}

/// Package dependency file types.
#[derive(Debug, Copy, Clone, strum_macros::EnumIter)]
enum DependencyFileType {
    PipfileLock,
}

impl DependencyFileType {
    /// Return file name associated with dependency type.
    pub fn file_name(&self) -> std::path::PathBuf {
        match self {
            Self::PipfileLock => std::path::PathBuf::from("Pipfile.lock"),
        }
    }
}

/// Package dependency file type and file path.
#[derive(Debug, Clone)]
struct DependencyFile {
    r#type: DependencyFileType,
    path: std::path::PathBuf,
}

/// Returns a vector of identified package dependency definition files.
///
/// Walks up the directory tree directory tree until the first positive result is found.
fn identify_dependency_files(working_directory: &std::path::Path) -> Option<Vec<DependencyFile>> {
    assert!(working_directory.is_absolute());
    let mut working_directory = working_directory.to_path_buf();

    loop {
        // If at least one target is found, assume package is present.
        let mut found_dependency_file = false;

        let mut dependency_files: Vec<DependencyFile> = Vec::new();
        for dependency_file_type in DependencyFileType::iter() {
            let target_absolute_path = working_directory.join(dependency_file_type.file_name());
            if target_absolute_path.is_file() {
                found_dependency_file = true;
                dependency_files.push(DependencyFile {
                    r#type: dependency_file_type,
                    path: target_absolute_path,
                })
            }
        }
        if found_dependency_file {
            return Some(dependency_files);
        }

        // No need to move further up the directory tree after this loop.
        if working_directory == std::path::Path::new("/") {
            break;
        }

        // Move further up the directory tree.
        working_directory.pop();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use thirdpass_core::extension::{Extension, FromLib};

    struct TempProject {
        root: std::path::PathBuf,
    }

    impl TempProject {
        fn new(label: &str) -> Result<Self> {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "thirdpass-py-{}-{}-{}",
                label,
                std::process::id(),
                timestamp
            ));
            std::fs::create_dir_all(&root)?;
            Ok(Self { root })
        }

        fn path(&self) -> &std::path::Path {
            &self.root
        }
    }

    impl Drop for TempProject {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn review_target_policy_includes_python_lockfiles() {
        let policy = PyExtension::new().review_target_policy();

        assert!(!policy.excludes_exact_path("Pipfile.lock"));
        assert!(!policy.excludes_exact_path("poetry.lock"));
        assert!(!policy.excludes_exact_path("uv.lock"));
        assert!(!policy.excludes_exact_path("pdm.lock"));
        assert!(!policy.excludes_exact_path("pyproject.toml"));
        assert!(!policy.excludes_exact_path("setup.py"));
        assert!(!policy.excludes_exact_path("requirements.txt"));
        assert!(!policy.excludes_exact_path("PKG-INFO"));
    }

    #[test]
    fn file_defined_dependencies_parse_pipfile_lock_from_child_directory() -> Result<()> {
        let project = TempProject::new("file-defined-dependencies")?;
        let nested = project.path().join("src").join("package");
        std::fs::create_dir_all(&nested)?;

        let pipfile_lock_path = project.path().join("Pipfile.lock");
        std::fs::write(
            &pipfile_lock_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "_meta": {},
                "default": {
                    "requests": {
                        "version": "==2.32.3"
                    }
                },
                "develop": {
                    "pytest": {
                        "version": "==8.3.4"
                    }
                }
            }))?,
        )?;

        let extension = PyExtension::new();
        let extension_args = Vec::new();
        let groups = extension.identify_file_defined_dependencies(&nested, &extension_args)?;

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].path, pipfile_lock_path);
        assert_eq!(groups[0].registry_host_name, "pypi.org");
        assert_dependency(&groups[0].dependencies, "requests", "2.32.3");
        assert_dependency(&groups[0].dependencies, "pytest", "8.3.4");
        Ok(())
    }

    fn assert_dependency(
        dependencies: &[thirdpass_core::extension::Dependency],
        name: &str,
        version: &str,
    ) {
        assert!(
            dependencies
                .iter()
                .any(|dependency| dependency.name == name
                    && dependency.version == Ok(version.into())),
            "expected dependency {}@{} in {:?}",
            name,
            version,
            dependencies
        );
    }
}
