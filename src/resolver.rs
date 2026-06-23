//! pip-backed package dependency resolution.
//!
//! Python packages declare version ranges, environment markers, extras, and
//! index-specific availability rather than one universal registry dependency
//! tree. This module asks pip to resolve the package in the caller's configured
//! environment and reads the exact package versions from pip's report output.

use anyhow::{format_err, Context, Result};

const PYTHON_ENV_VAR: &str = "THIRDPASS_PYTHON";
const SUPPORTED_RESOLVER_ARGS: &str = "--index-url, -i, --extra-index-url, \
    --find-links, -f, --trusted-host, --proxy, --cert, --client-cert, \
    --timeout, --retries, --python-version, --platform, --implementation, \
    --abi, --only-binary, --no-binary, --constraint, -c, --no-index, \
    --pre, --prefer-binary, --no-build-isolation, --no-cache-dir";

/// Resolve a package release into the exact PyPI package versions pip selects.
pub(crate) fn identify_package_dependencies(
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

fn resolve_package_dependencies(
    package_name: &str,
    package_version: &Option<&str>,
    extension_args: &[String],
) -> Result<serde_json::Value> {
    let resolver_args = resolver_args_from_extension_args(extension_args)?;
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
            .args(&resolver_args)
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

fn resolver_args_from_extension_args(extension_args: &[String]) -> Result<Vec<String>> {
    let mut resolver_args = Vec::new();
    let mut index = 0;
    while index < extension_args.len() {
        let arg = &extension_args[index];
        let spec = resolver_arg_spec(arg).ok_or(format_err!(
            "Unsupported Python resolver argument: {}. Supported pip arguments: {}",
            arg,
            SUPPORTED_RESOLVER_ARGS
        ))?;
        resolver_args.push(arg.clone());

        if spec == ResolverArgSpec::Value && !has_inline_value(arg) {
            index += 1;
            let value = extension_args.get(index).ok_or(format_err!(
                "Python resolver argument {} requires a value.",
                arg
            ))?;
            if value.starts_with('-') {
                return Err(format_err!(
                    "Python resolver argument {} requires a value.",
                    arg
                ));
            }
            resolver_args.push(value.clone());
        }

        index += 1;
    }

    Ok(resolver_args)
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ResolverArgSpec {
    Flag,
    Value,
}

fn resolver_arg_spec(arg: &str) -> Option<ResolverArgSpec> {
    let option_name = arg.split_once('=').map(|(name, _)| name).unwrap_or(arg);
    match option_name {
        "--index-url" | "-i" | "--extra-index-url" | "--find-links" | "-f" | "--trusted-host"
        | "--proxy" | "--cert" | "--client-cert" | "--timeout" | "--retries"
        | "--python-version" | "--platform" | "--implementation" | "--abi" | "--only-binary"
        | "--no-binary" | "--constraint" | "-c" => Some(ResolverArgSpec::Value),
        "--no-index" | "--pre" | "--prefer-binary" | "--no-build-isolation" | "--no-cache-dir" => {
            Some(ResolverArgSpec::Flag)
        }
        _ => None,
    }
}

fn has_inline_value(arg: &str) -> bool {
    arg.starts_with("--") && arg.contains('=')
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
    if let Ok(python) = std::env::var(PYTHON_ENV_VAR) {
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
        registry_host_name: crate::pipfile::get_registry_host_name(),
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

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TempProject {
        root: std::path::PathBuf,
    }

    impl TempProject {
        fn new(label: &str) -> Result<Self> {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "thirdpass-py-resolver-test-{}-{}-{}",
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

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.as_ref() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
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
    fn resolver_args_accept_supported_pip_knobs() -> Result<()> {
        let args = resolver_args_from_extension_args(&[
            "--index-url=https://example.invalid/simple".to_string(),
            "--python-version".to_string(),
            "3.12".to_string(),
            "--platform".to_string(),
            "manylinux2014_x86_64".to_string(),
            "--implementation".to_string(),
            "cp".to_string(),
            "--abi".to_string(),
            "cp312".to_string(),
            "--only-binary".to_string(),
            ":all:".to_string(),
            "--pre".to_string(),
        ])?;

        assert_eq!(
            args,
            vec![
                "--index-url=https://example.invalid/simple",
                "--python-version",
                "3.12",
                "--platform",
                "manylinux2014_x86_64",
                "--implementation",
                "cp",
                "--abi",
                "cp312",
                "--only-binary",
                ":all:",
                "--pre"
            ]
        );
        Ok(())
    }

    #[test]
    fn resolver_args_reject_unsupported_options() {
        let error = resolver_args_from_extension_args(&["--target".to_string()])
            .expect_err("expected unsupported argument to fail");

        assert!(error
            .to_string()
            .contains("Unsupported Python resolver argument: --target"));
    }

    #[test]
    fn resolver_args_require_values_for_value_options() {
        let error = resolver_args_from_extension_args(&["--index-url".to_string()])
            .expect_err("expected missing value to fail");

        assert!(error
            .to_string()
            .contains("Python resolver argument --index-url requires a value."));
    }

    #[test]
    fn canonical_package_name_normalizes_pep_503_names() {
        assert_eq!(
            canonical_package_name("Example.Package__Name"),
            "example-package-name"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolver_command_reads_fake_pip_report() -> Result<()> {
        let _lock = ENV_LOCK.lock().expect("resolver env lock poisoned");
        let project = TempProject::new("fake-pip")?;
        let fake_python = write_fake_python(project.path())?;
        let _python_env = EnvVarGuard::set(PYTHON_ENV_VAR, &fake_python);
        let extension_args = vec![
            "--index-url".to_string(),
            "https://example.invalid/simple".to_string(),
        ];

        let dependencies =
            identify_package_dependencies("sample-package", &Some("1.0.0"), &extension_args)?;

        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].package_version, Ok("1.0.0".to_string()));
        assert_dependency(&dependencies[0].dependencies, "dependency", "2.0.0");
        Ok(())
    }

    #[cfg(unix)]
    fn write_fake_python(directory: &std::path::Path) -> Result<std::path::PathBuf> {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join("fake-python");
        std::fs::write(
            &path,
            r#"#!/bin/sh
report=""
saw_index_url=0
saw_requirement=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --report)
            shift
            report="$1"
            ;;
        --index-url)
            shift
            if [ "$1" = "https://example.invalid/simple" ]; then
                saw_index_url=1
            fi
            ;;
        sample-package==1.0.0)
            saw_requirement=1
            ;;
    esac
    shift
done
if [ -z "$report" ] || [ "$saw_index_url" -ne 1 ] || [ "$saw_requirement" -ne 1 ]; then
    exit 1
fi
cat > "$report" <<'JSON'
{
  "install": [
    {
      "requested": true,
      "metadata": {
        "name": "sample-package",
        "version": "1.0.0"
      }
    },
    {
      "requested": false,
      "metadata": {
        "name": "dependency",
        "version": "2.0.0"
      }
    }
  ]
}
JSON
"#,
        )?;
        let mut permissions = std::fs::metadata(&path)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions)?;
        Ok(path)
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
