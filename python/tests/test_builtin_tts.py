"""Contract tests for builtin TTS implementations without heavyweight model imports."""

from __future__ import annotations

from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from neuromita_ai_sdk import InstallPlan
from neuromita_ai_sdk.host import PluginHost
from neuromita_voice import plans
from neuromita_voice.runtime import VoicePlugin


class BuiltinTtsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.host = PluginHost(Path(__file__).resolve().parents[1] / "plugins" / "builtin")

    def test_all_legacy_tts_modes_discover_and_provide_plans(self):
        found = {x["id"] for x in self.host.discover() if x["discovery_status"] == "available"}
        self.assertTrue(set(plans.VARIANTS).issubset(found))
        for plugin_id in plans.VARIANTS:
            with self.subTest(plugin_id=plugin_id):
                result = self.host.dispatch("install_plan", {"plugin_id": plugin_id})
                self.assertEqual(result["backend"], plans.backend(plugin_id))
                self.assertIn("numpy==1.26.0", result["requirements"])
                self.assertIn("scipy==1.12.0", result["requirements"])
                self.assertTrue(self.host.dispatch("settings_schema", {"plugin_id": plugin_id}))
                probe = self.host.dispatch("probe_install", {"plugin_id": plugin_id})
                self.assertFalse(probe["installed"])

    def test_rvc_variants_use_explicit_asset_extensions_and_pitch(self):
        for plugin_id in plans.VARIANTS:
            if "rvc" not in plugin_id:
                continue
            with self.subTest(plugin_id=plugin_id):
                assets = plans.plan(plugin_id).assets
                required = [a.relative_path for a in assets if a.role == "rvc_voice_weights"]
                self.assertEqual(len(required), 1)
                self.assertTrue(required[0].endswith(".onnx" if plugin_id.endswith("onnx") else ".pth"))
                self.assertEqual(plans.settings(plugin_id)["pitch"]["default"], 6.0)

    def test_tts_has_no_hidden_voice_fallback(self):
        plugin = VoicePlugin("f5_tts_ru")
        with patch.object(plugin, "probe_install", return_value={"installed": True}), patch.dict("sys.modules", {}, clear=False):
            with self.assertRaises((ImportError, FileNotFoundError, ModuleNotFoundError)):
                plugin.initialize({"device": "cpu"})
        self.assertFalse(plugin.initialized)

    def test_uninstalled_mode_is_not_reported_ready(self):
        plugin = VoicePlugin("omnivoice")
        with tempfile.TemporaryDirectory() as root, patch.dict("os.environ", {"NEUROMITA_AI_MODELS_DIR": root}):
            with self.assertRaisesRegex(RuntimeError, "missing"):
                plugin.initialize({})
        self.assertFalse(plugin.initialized)

    def test_f5_speaker_invalid_legacy_setting_is_rejected(self):
        plugin = VoicePlugin("f5_tts_ru")
        with self.assertRaisesRegex(ValueError, "unknown"):
            plugin.initialize({"speaker": "does-not-exist"})


if __name__ == "__main__":
    unittest.main()
