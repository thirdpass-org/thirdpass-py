# thirdpass-py

Python package extension for Thirdpass.

This repo contains the Thirdpass extension that understands Python
packages and Python dependency files. It can be used by the Thirdpass
CLI to discover Python dependencies, fetch package metadata from PyPI,
and resolve dependency trees for package review.

## Dependency-tree review

`thirdpass review <package> <version> --deps --extension py` uses
pip's resolver in dry-run mode and reads pip's JSON report. The
extension returns the exact package versions pip selected for the
requested package and its transitive dependencies.

Python resolution is environment-sensitive. The result can change with
the pip version, Python version, target platform, package indexes,
environment markers, and available wheels or source distributions. Set
`THIRDPASS_PYTHON` to choose the Python executable used for
`python -m pip`; otherwise the extension tries `python3 -m pip`,
`pip3`, and then `pip`.

Arguments after `--` are validated and passed to pip. Supported
resolver arguments are `--index-url`/`-i`, `--extra-index-url`,
`--find-links`/`-f`,
`--trusted-host`, `--proxy`, `--cert`, `--client-cert`, `--timeout`,
`--retries`, `--python-version`, `--platform`, `--implementation`,
`--abi`, `--only-binary`, `--no-binary`, `--constraint`/`-c`,
`--no-index`, `--pre`, `--prefer-binary`, `--no-build-isolation`, and
`--no-cache-dir`.
