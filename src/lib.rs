use anyhow::{format_err, Context, Result};
use std::io::Read;
use strum::IntoEnumIterator;

mod pipfile;

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
        let report = resolve_package_dependencies(package_name, package_version, extension_args)?;
        let dependencies = package_dependencies_from_pip_report(
            &report,
            package_name,
            package_version.as_ref().copied(),
        )?;
        Ok(vec![dependencies])
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

fn resolve_package_dependencies(
    package_name: &str,
    package_version: &Option<&str>,
    extension_args: &[String],
) -> Result<serde_json::Value> {
    let temp_directory = TempResolverDirectory::new("identify-package-dependencies")?;
    let report_path = temp_directory.path().join("pip-report.json");
    let package_requirement = package_requirement(package_name, package_version);

    let mut attempt_errors = Vec::new();
    for command in pip_resolver_commands() {
        let mut resolver = std::process::Command::new(&command.program);
        resolver
            .args(&command.prefix_args)
            .arg("install")
            .arg("--disable-pip-version-check")
            .arg("--no-input")
            .arg("--dry-run")
            .arg("--ignore-installed")
            .arg("--report")
            .arg(&report_path)
            .args(extension_args)
            .arg(&package_requirement)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .current_dir(temp_directory.path());

        match resolver.output() {
            Ok(output) if output.status.success() => {
                let report = std::fs::read_to_string(&report_path).context(format!(
                    "Failed to read pip resolver report: {}",
                    report_path.display()
                ))?;
                return serde_json::from_str(&report)
                    .context("Failed to parse pip resolver report.");
            }
            Ok(output) => {
                attempt_errors.push(format!(
                    "{} failed:\n{}",
                    command.display(),
                    command_output(&output)
                ));
            }
            Err(error) => {
                attempt_errors.push(format!("{} failed to start: {}", command.display(), error));
            }
        }
    }

    Err(format_err!(
        "Python package resolver failed for {}:\n{}",
        package_requirement,
        attempt_errors.join("\n")
    ))
}

fn package_requirement(package_name: &str, package_version: &Option<&str>) -> String {
    match package_version {
        Some(package_version) => format!("{}=={}", package_name, package_version),
        None => package_name.to_string(),
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ResolverCommand {
    program: String,
    prefix_args: Vec<String>,
}

impl ResolverCommand {
    fn display(&self) -> String {
        let mut parts = vec![self.program.clone()];
        parts.extend(self.prefix_args.clone());
        parts.join(" ")
    }
}

fn pip_resolver_commands() -> Vec<ResolverCommand> {
    if let Ok(python) = std::env::var("THIRDPASS_PYTHON") {
        return vec![ResolverCommand {
            program: python,
            prefix_args: vec!["-m".to_string(), "pip".to_string()],
        }];
    }

    vec![
        ResolverCommand {
            program: "python3".to_string(),
            prefix_args: vec!["-m".to_string(), "pip".to_string()],
        },
        ResolverCommand {
            program: "pip3".to_string(),
            prefix_args: Vec::new(),
        },
        ResolverCommand {
            program: "pip".to_string(),
            prefix_args: Vec::new(),
        },
    ]
}

fn command_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut message = String::new();
    if !stdout.trim().is_empty() {
        message.push_str("stdout:\n");
        message.push_str(stdout.trim());
        message.push('\n');
    }
    if !stderr.trim().is_empty() {
        message.push_str("stderr:\n");
        message.push_str(stderr.trim());
    }
    if message.is_empty() {
        "resolver produced no output".to_string()
    } else {
        message
    }
}

fn package_dependencies_from_pip_report(
    report: &serde_json::Value,
    package_name: &str,
    fallback_package_version: Option<&str>,
) -> Result<thirdpass_core::extension::PackageDependencies> {
    let resolved_packages = resolved_packages_from_pip_report(report)?;
    let target_package = select_target_package(&resolved_packages, package_name);
    let package_version = target_package
        .map(|package| package.version.clone())
        .or_else(|| fallback_package_version.map(ToOwned::to_owned))
        .ok_or(format_err!(
            "Failed to find target package in pip resolver report."
        ))?;
    let target_name = canonical_package_name(package_name);

    let mut dependencies =
        std::collections::BTreeMap::<String, thirdpass_core::extension::Dependency>::new();
    for package in resolved_packages {
        let canonical_name = canonical_package_name(&package.name);
        if canonical_name == target_name {
            continue;
        }
        dependencies.insert(
            canonical_name,
            thirdpass_core::extension::Dependency {
                name: package.name,
                version: Ok(package.version),
            },
        );
    }

    Ok(thirdpass_core::extension::PackageDependencies {
        package_version: Ok(package_version),
        registry_host_name: pipfile::get_registry_host_name(),
        dependencies: dependencies.into_values().collect(),
    })
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ResolvedPackage {
    name: String,
    version: String,
    requested: bool,
}

fn resolved_packages_from_pip_report(report: &serde_json::Value) -> Result<Vec<ResolvedPackage>> {
    let install_entries = report["install"]
        .as_array()
        .ok_or(format_err!("Failed to parse pip report install section."))?;

    let mut packages = Vec::new();
    for entry in install_entries {
        let metadata = entry["metadata"]
            .as_object()
            .ok_or(format_err!("Failed to parse pip report metadata section."))?;
        let name = metadata["name"]
            .as_str()
            .ok_or(format_err!("Failed to parse pip report package name."))?;
        let version = metadata["version"]
            .as_str()
            .ok_or(format_err!("Failed to parse pip report package version."))?;
        let requested = entry["requested"].as_bool().unwrap_or(false);
        packages.push(ResolvedPackage {
            name: name.to_string(),
            version: version.to_string(),
            requested,
        });
    }

    Ok(packages)
}

fn select_target_package<'a>(
    packages: &'a [ResolvedPackage],
    package_name: &str,
) -> Option<&'a ResolvedPackage> {
    let package_name = canonical_package_name(package_name);
    packages
        .iter()
        .find(|package| package.requested && canonical_package_name(&package.name) == package_name)
        .or_else(|| {
            packages
                .iter()
                .find(|package| canonical_package_name(&package.name) == package_name)
        })
}

fn canonical_package_name(name: &str) -> String {
    let mut canonical_name = String::new();
    let mut last_was_separator = false;
    for character in name.chars() {
        if character == '-' || character == '_' || character == '.' {
            if !last_was_separator {
                canonical_name.push('-');
                last_was_separator = true;
            }
        } else {
            canonical_name.push(character.to_ascii_lowercase());
            last_was_separator = false;
        }
    }
    canonical_name
}

struct TempResolverDirectory {
    path: std::path::PathBuf,
}

impl TempResolverDirectory {
    fn new(label: &str) -> Result<Self> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "thirdpass-py-resolver-{}-{}-{}",
            label,
            std::process::id(),
            timestamp
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempResolverDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
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

    #[test]
    fn package_dependencies_parse_pip_report() -> Result<()> {
        let report = serde_json::json!({
            "version": "1",
            "pip_version": "25.1",
            "install": [
                {
                    "requested": true,
                    "metadata": {
                        "name": "requests",
                        "version": "2.32.3"
                    }
                },
                {
                    "requested": false,
                    "metadata": {
                        "name": "certifi",
                        "version": "2025.1.31"
                    }
                },
                {
                    "requested": false,
                    "metadata": {
                        "name": "urllib3",
                        "version": "2.3.0"
                    }
                }
            ]
        });

        let dependencies = package_dependencies_from_pip_report(&report, "requests", None)?;

        assert_eq!(dependencies.package_version, Ok("2.32.3".to_string()));
        assert_eq!(dependencies.registry_host_name, "pypi.org");
        assert_eq!(dependencies.dependencies.len(), 2);
        assert_dependency(&dependencies.dependencies, "certifi", "2025.1.31");
        assert_dependency(&dependencies.dependencies, "urllib3", "2.3.0");
        assert!(dependencies
            .dependencies
            .iter()
            .all(|dependency| dependency.name != "requests"));
        Ok(())
    }

    #[test]
    fn package_dependencies_match_canonical_target_name() -> Result<()> {
        let report = serde_json::json!({
            "install": [
                {
                    "requested": true,
                    "metadata": {
                        "name": "sample-package",
                        "version": "1.0.0"
                    }
                },
                {
                    "metadata": {
                        "name": "dependency_pkg",
                        "version": "2.0.0"
                    }
                }
            ]
        });

        let dependencies = package_dependencies_from_pip_report(&report, "sample.package", None)?;

        assert_eq!(dependencies.package_version, Ok("1.0.0".to_string()));
        assert_eq!(dependencies.dependencies.len(), 1);
        assert_dependency(&dependencies.dependencies, "dependency_pkg", "2.0.0");
        Ok(())
    }

    #[test]
    fn package_dependencies_use_given_version_without_report_target() -> Result<()> {
        let report = serde_json::json!({
            "install": [
                {
                    "metadata": {
                        "name": "dependency",
                        "version": "2.0.0"
                    }
                }
            ]
        });

        let dependencies = package_dependencies_from_pip_report(&report, "target", Some("1.0.0"))?;

        assert_eq!(dependencies.package_version, Ok("1.0.0".to_string()));
        assert_dependency(&dependencies.dependencies, "dependency", "2.0.0");
        Ok(())
    }

    #[test]
    fn canonical_package_name_normalizes_pep_503_names() {
        assert_eq!(
            canonical_package_name("Example.Package__Name"),
            "example-package-name"
        );
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
