"""Static manifest discovery, executable-plugin isolation and installer routing."""

import json
from pathlib import Path
import tempfile
import unittest

from neuromita_ai_sdk.host import PluginHost
from neuromita_ai_sdk.installer import execute


PLUGIN_CODE = '''from neuromita_ai_sdk import InstallPlan
class Plugin:
    def describe(self): return {"kind": "tts"}
    def install_plan(self): return InstallPlan()
    def probe_install(self): return {"installed": True}
    def settings_schema(self): return {}
    def initialize(self, settings): return {"model_ready": True}
    def infer(self, input_data): return {"ok": True}
    def unload(self): pass
'''


class PluginDiscoveryTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def fixture(self, source, plugin_id, **overrides):
        directory = self.root / source / plugin_id
        directory.mkdir(parents=True)
        manifest = {
            "id": plugin_id,
            "name": "Fixture",
            "version": "1.2.3",
            "publisher": "tests",
            "permissions": [],
            "api_version": 1,
            "kind": "tts",
            "entrypoint": "plugin.py",
        }
        manifest.update(overrides)
        (directory / "plugin.json").write_text(json.dumps(manifest), encoding="utf-8")
        (directory / "plugin.py").write_text(PLUGIN_CODE, encoding="utf-8")
        return directory

    def test_flat_builtin_and_community_plugins_are_discovered_without_imports(self):
        self.fixture("builtin", "bundled")
        third_party = self.fixture("community", "third_party")
        (third_party / "plugin.py").write_text("raise RuntimeError('not imported on discovery')")
        self.fixture("", "legacy")
        host = PluginHost(self.root)
        result = {item["id"]: item for item in host.discover()}
        self.assertEqual(set(result), {"bundled", "third_party", "legacy"})
        self.assertEqual(result["bundled"]["source"], "builtin")
        self.assertEqual(result["third_party"]["source"], "community")
        self.assertEqual(result["third_party"]["discovery_status"], "available")
        self.assertEqual(host.dispatch("initialize", {"plugin_id": "bundled"}), {"model_ready": True})
        host.dispatch("unload", {"kind": "tts"})
        with self.assertRaisesRegex(RuntimeError, "not imported"):
            host.dispatch("initialize", {"plugin_id": "third_party"})

    def test_duplicate_plugin_ids_are_rejected_instead_of_shadowing(self):
        self.fixture("builtin", "same")
        self.fixture("community", "same")
        host = PluginHost(self.root)
        self.assertEqual([item["discovery_status"] for item in host.discover()], ["invalid", "invalid"])
        with self.assertRaisesRegex(ValueError, "ambiguous"):
            host.dispatch("describe", {"plugin_id": "same"})

    def test_symlink_collision_does_not_make_a_real_plugin_selectable(self):
        original = self.fixture("builtin", "same")
        (self.root / "community").mkdir(exist_ok=True)
        (self.root / "community" / "same").symlink_to(original, target_is_directory=True)
        host = PluginHost(self.root)
        self.assertEqual([item["discovery_status"] for item in host.discover()], ["invalid", "invalid"])
        with self.assertRaisesRegex(ValueError, "ambiguous"):
            host.dispatch("describe", {"plugin_id": "same"})

    def test_unapproved_permissions_are_not_imported_or_executed(self):
        directory = self.fixture("community", "restricted", permissions=["network"])
        (directory / "plugin.py").write_text("raise RuntimeError('must never import')")
        host = PluginHost(self.root)
        self.assertEqual(host.discover()[0]["discovery_status"], "approval_required")
        for op in ("describe", "install_plan", "initialize"):
            with self.assertRaisesRegex(ValueError, "explicit approval"):
                host.dispatch(op, {"plugin_id": "restricted"})

    def test_invalid_manifest_only_invalidates_its_plugin(self):
        self.fixture("builtin", "valid")
        self.fixture("community", "invalid", permissions=["arbitrary_python"])
        self.fixture("community", "missing", version=None)
        host = PluginHost(self.root)
        statuses = {item["id"]: item["discovery_status"] for item in host.discover()}
        self.assertEqual(statuses, {"valid": "available", "invalid": "invalid", "missing": "invalid"})
        self.assertEqual(host.dispatch("describe", {"plugin_id": "valid"}), {"kind": "tts"})

    def test_symlinks_and_oversized_manifests_are_rejected(self):
        valid = self.fixture("builtin", "valid")
        another = self.fixture("community", "too_big")
        (another / "plugin.json").write_text(" " * 65537)
        (self.root / "community" / "linked").symlink_to(valid, target_is_directory=True)
        host = PluginHost(self.root)
        statuses = {item["id"]: item["discovery_status"] for item in host.discover()}
        self.assertEqual(statuses, {"valid": "available", "linked": "invalid", "too_big": "invalid"})
        with self.assertRaises(ValueError):
            host.dispatch("describe", {"plugin_id": "linked"})

    def test_nested_install_constraints_are_enforced_at_initialize(self):
        directory = self.fixture("community", "restricted_setting")
        (directory / "plugin.py").write_text(PLUGIN_CODE.replace(
            "def settings_schema(self): return {}",
            "def settings_schema(self): return {'precision': {'type': 'string'}}"
        ))
        (directory / "installplan.py").write_text('''from neuromita_ai_sdk import InstallPlan
class InstallContract:
    def settings_schema(self): return {'precision': {'type': 'string'}}
    def probe_install(self): return {'installed': True}
    def install_plan(self): return InstallPlan()
    def configuration_constraints(self, context):
        return [{'field': 'settings.precision', 'value': 'fp16', 'severity': 'forbidden',
                 'code': 'forbidden_precision', 'message': 'Cannot use fp16'}]
''')
        host = PluginHost(self.root)
        with self.assertRaisesRegex(ValueError, "forbidden_precision"):
            host.dispatch("initialize", {"plugin_id": "restricted_setting", "settings": {"precision": "fp16"}})
        self.assertEqual(host.active_by_kind, {})
        self.assertTrue(host.dispatch("initialize", {
            "plugin_id": "restricted_setting", "settings": {"precision": "fp32"}
        })["model_ready"])
        host.dispatch("unload", {"kind": "tts"})

    def test_nested_installer_uses_exact_discovered_plugin(self):
        directory = self.fixture("community", "convert")
        (directory / "installplan.py").write_text('''from neuromita_ai_sdk import InstallPlan, AssetHook
class InstallContract:
    def settings_schema(self): return {}
    def probe_install(self): return {"installed": False}
    def install_plan(self): return InstallPlan(asset_hooks=(AssetHook("convert", "source"),))
''')
        (directory / "install.py").write_text('''def execute(hook_id, source_root, output_root, settings):
    (output_root / "converted.txt").write_text(hook_id)
''')
        source = self.root / "input"
        output = self.root / "output"
        source.mkdir()
        output.mkdir()
        execute(self.root, "convert", "convert", "source", source, output, {})
        self.assertEqual((output / "converted.txt").read_text(), "convert")
        with self.assertRaisesRegex(ValueError, "not declared"):
            execute(self.root, "convert", "other", "source", source, output, {})


if __name__ == "__main__":
    unittest.main()
