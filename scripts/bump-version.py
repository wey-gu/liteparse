#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///

"""Bump the version of LiteParse packages.

Updates versions across:
- crates/liteparse/Cargo.toml
- crates/liteparse-napi/Cargo.toml (and its liteparse dep)
- crates/liteparse-python/Cargo.toml (and its liteparse dep)
- crates/liteparse-wasm/Cargo.toml
- packages/node/package.json
- packages/wasm/package.json
- packages/python/pyproject.toml  (PEP 440 form, e.g. 2.0.1b1)

Excludes pdfium and pdfium-sys crates.

Accepts versions in:
  - major.minor.patch                  e.g. 2.0.1
  - major.minor.patch-pre.number       e.g. 2.0.1-beta.1, 2.0.1-alpha.0, 2.0.1-rc.2

Usage:
  scripts/bump-version.py <version>                  # update all packages
  scripts/bump-version.py <version> --package <pkg>  # update only one
      where <pkg> is one of: core, node, python, wasm

Examples:
  scripts/bump-version.py 2.0.1
  scripts/bump-version.py 2.0.1-beta.1
  scripts/bump-version.py 2.1.0-rc.0 --package python
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

SEMVER_RE = re.compile(
    r"^(?P<major>\d+)\.(?P<minor>\d+)\.(?P<patch>\d+)"
    r"(?:-(?P<pre>alpha|beta|rc)\.(?P<num>\d+))?$"
)

PRE_TO_PEP440 = {"alpha": "a", "beta": "b", "rc": "rc"}


def parse_version(v: str) -> tuple[str, str | None, int | None]:
    """Return (base "X.Y.Z", pre_label or None, pre_num or None)."""
    m = SEMVER_RE.match(v)
    if not m:
        raise SystemExit(
            f"Invalid version: {v!r}. Expected X.Y.Z or X.Y.Z-(alpha|beta|rc).N"
        )
    base = f"{m['major']}.{m['minor']}.{m['patch']}"
    pre = m["pre"]
    num = int(m["num"]) if m["num"] is not None else None
    return base, pre, num


def to_semver(base: str, pre: str | None, num: int | None) -> str:
    if pre is None:
        return base
    return f"{base}-{pre}.{num}"


def to_pep440(base: str, pre: str | None, num: int | None) -> str:
    if pre is None:
        return base
    return f"{base}{PRE_TO_PEP440[pre]}{num}"


def replace_in_file(
    path: Path, pattern: str, replacement: str, *, expect: int = 1
) -> None:
    text = path.read_text()
    new_text, n = re.subn(pattern, replacement, text, count=expect if expect else 0)
    if expect and n != expect:
        raise SystemExit(
            f"{path}: expected {expect} replacement(s) for /{pattern}/, made {n}"
        )
    if n == 0:
        print(f"  (no changes in {path.relative_to(REPO_ROOT)})")
        return
    path.write_text(new_text)
    print(
        f"  updated {path.relative_to(REPO_ROOT)} ({n} change{'s' if n != 1 else ''})"
    )


def update_cargo_version(path: Path, new_version: str) -> None:
    # Update the first top-level `version = "..."` line (the [package] version).
    replace_in_file(
        path,
        r'(?m)^version\s*=\s*"[^"]+"',
        f'version = "{new_version}"',
        expect=1,
    )


def update_liteparse_dep(path: Path, new_version: str) -> None:
    # Update the inline `liteparse = { package = "liteparse", version = "..." }` line.
    replace_in_file(
        path,
        r'(liteparse\s*=\s*\{\s*package\s*=\s*"liteparse"\s*,\s*version\s*=\s*)"[^"]+"',
        rf'\1"{new_version}"',
        expect=1,
    )


def update_json_version(path: Path, new_version: str) -> None:
    # Update the top-level "version": "..." field in a package.json.
    replace_in_file(
        path,
        r'("version"\s*:\s*)"[^"]+"',
        rf'\1"{new_version}"',
        expect=1,
    )


def update_pyproject_version(path: Path, new_version: str) -> None:
    # Update the `version = "..."` field in pyproject.toml's [project] table.
    replace_in_file(
        path,
        r'(?m)^version\s*=\s*"[^"]+"',
        f'version = "{new_version}"',
        expect=1,
    )


def bump_core(semver: str) -> None:
    print("• core (crates/liteparse)")
    update_cargo_version(REPO_ROOT / "crates/liteparse/Cargo.toml", semver)
    # Also update the path-dep version pin used by napi/python crates so it
    # stays in sync with the core crate's own version.
    print("  syncing liteparse dep version in dependent crates")
    update_liteparse_dep(REPO_ROOT / "crates/liteparse-napi/Cargo.toml", semver)
    update_liteparse_dep(REPO_ROOT / "crates/liteparse-python/Cargo.toml", semver)


def bump_node(semver: str) -> None:
    print("• node (crates/liteparse-napi + packages/node)")
    update_cargo_version(REPO_ROOT / "crates/liteparse-napi/Cargo.toml", semver)
    update_json_version(REPO_ROOT / "packages/node/package.json", semver)


def bump_python(semver: str, pep440: str) -> None:
    print("• python (crates/liteparse-python + packages/python)")
    update_cargo_version(REPO_ROOT / "crates/liteparse-python/Cargo.toml", semver)
    update_pyproject_version(REPO_ROOT / "packages/python/pyproject.toml", pep440)


def bump_wasm(semver: str) -> None:
    print("• wasm (crates/liteparse-wasm + packages/wasm)")
    update_cargo_version(REPO_ROOT / "crates/liteparse-wasm/Cargo.toml", semver)
    update_json_version(REPO_ROOT / "packages/wasm/package.json", semver)


PACKAGES = ("core", "node", "python", "wasm")


def main() -> int:
    parser = argparse.ArgumentParser(description="Bump LiteParse package versions.")
    parser.add_argument("version", help="Target version (e.g. 2.0.1 or 2.0.1-beta.1)")
    parser.add_argument(
        "--package",
        choices=PACKAGES,
        help="Update only one package group (default: all)",
    )
    args = parser.parse_args()

    base, pre, num = parse_version(args.version)
    semver = to_semver(base, pre, num)
    pep440 = to_pep440(base, pre, num)

    print(f"Target semver:  {semver}")
    print(f"Target PEP 440: {pep440}")
    print()

    targets = (args.package,) if args.package else PACKAGES
    for pkg in targets:
        if pkg == "core":
            bump_core(semver)
        elif pkg == "node":
            bump_node(semver)
        elif pkg == "python":
            bump_python(semver, pep440)
        elif pkg == "wasm":
            bump_wasm(semver)

    print("\nDone.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
