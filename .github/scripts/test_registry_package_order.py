import unittest

from registry_package_order import package_order


def package(name, dependencies=(), publish=None):
    return {
        "name": name,
        "publish": ["phoxal"] if publish is None else publish,
        "dependencies": [{"name": dependency, "kind": kind} for dependency, kind in dependencies],
    }


class PackageOrderTests(unittest.TestCase):
    def test_transitive_build_dependencies_and_dev_cycles(self):
        metadata = {"packages": [
            package("last", [("middle", "build")]),
            package("middle", [("root", None)]),
            package("root", [("last", "dev"), ("public", None)]),
            package("public", publish=["crates-io"]),
        ]}
        self.assertEqual(package_order(metadata), [("root", False), ("middle", True), ("last", True)])

    def test_normal_cycle_is_refused(self):
        with self.assertRaisesRegex(ValueError, "cycle"):
            package_order({"packages": [package("a", [("b", None)]), package("b", [("a", None)])]})

    def test_empty_release_is_refused(self):
        with self.assertRaisesRegex(ValueError, "no alternate-registry"):
            package_order({"packages": []})


if __name__ == "__main__":
    unittest.main()
