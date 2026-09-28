#!/usr/bin/env python3
"""Plans dependency-ready publication groups for the framework workspace.

Prints JSON: {"groups": [[{name, version, kind, dir, deps}, ...], ...]}
in Kahn order over intra-workspace dependencies, restricted to packages
that publish to the phoxal registry. Publication kinds are derived from
the workspace layout; a new publishable location must be added here
explicitly.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


def kind_of(manifest: Path) -> str:
    parts = manifest.parts
    if "services" in parts:
        return "service"
    if "components" in parts:
        return "component"
    name = manifest.name
    parent = manifest.parent.name
    if name == "Cargo.toml" and parent == "macros" and "phoxal" in parts:
        return "proc-macro"
    if name == "Cargo.toml" and parent == "cargo" and "phoxal" in parts:
        return "tool"
    if name == "Cargo.toml" and parent in {"build", "phoxal", "supervisor"}:
        return "library"
    raise SystemExit(f"no publication kind for manifest {manifest}")


def main() -> None:
    no_deps = json.loads(subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        capture_output=True, text=True, check=True,
    ).stdout)
    packages: dict[str, dict] = {}
    for package in no_deps["packages"]:
        publish = package.get("publish")
        if isinstance(publish, list) and "phoxal" in publish:
            manifest = Path(package["manifest_path"])
            packages[package["name"]] = {
                "version": package["version"],
                "kind": kind_of(manifest),
                "dir": str(manifest.parent),
                "deps": [],
            }
    if not packages:
        raise SystemExit("no publishable packages found")

    full = json.loads(subprocess.run(
        ["cargo", "metadata", "--format-version", "1"],
        capture_output=True, text=True, check=True,
    ).stdout)
    name_by_id = {package["id"]: package["name"] for package in full["packages"]}
    for node in full["resolve"]["nodes"]:
        name = name_by_id.get(node["id"])
        if name not in packages:
            continue
        packages[name]["deps"] = sorted({
            name_by_id[dependency]
            for dependency in node["dependencies"]
            if name_by_id.get(dependency) in packages
            and name_by_id[dependency] != name
        })

    remaining = set(packages)
    groups: list[list[dict]] = []
    while remaining:
        ready = sorted(
            name for name in remaining
            if all(dep not in remaining for dep in packages[name]["deps"])
        )
        if not ready:
            raise SystemExit(f"cyclic publication dependency among {sorted(remaining)}")
        groups.append([{**packages[name], "name": name} for name in ready])
        remaining -= set(ready)

    json.dump({"groups": groups}, sys.stdout)


if __name__ == "__main__":
    main()
