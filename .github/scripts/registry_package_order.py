"""Order alternate-registry packages by their normal and build dependencies."""

import json
import sys


def package_order(metadata):
    packages = {
        package["name"]: package
        for package in metadata["packages"]
        if package.get("publish") == ["phoxal"]
    }
    dependencies = {
        name: {
            dependency["name"]
            for dependency in package["dependencies"]
            if dependency.get("kind") != "dev" and dependency["name"] in packages
        }
        for name, package in packages.items()
    }
    pending = set(packages)
    result = []
    while pending:
        ready = sorted(name for name in pending if not dependencies[name] & pending)
        if not ready:
            raise ValueError("alternate-registry package dependency cycle: " + ", ".join(sorted(pending)))
        for name in ready:
            result.append((name, bool(dependencies[name])))
            pending.remove(name)
    if not result:
        raise ValueError("no alternate-registry packages found")
    return result


if __name__ == "__main__":
    for name, dependent in package_order(json.load(sys.stdin)):
        print(f"{name}\t{int(dependent)}")
