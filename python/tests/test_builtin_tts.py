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
                self.assertIsNone(result["backend"])
                self.assertEqual(result["supported_backends"], list(plans.supported_backends(plugin_id)))
                self.assertIn("numpy==1.26.0", result["requirements"])
                self.assertIn("scipy==1.12.0", result["requirements"])
                self.assertEqual(result["supported_python_minors"], ["3.11", "3.12"])
                self.assertEqual(plans.settings(plugin_id)["device"]["default"], "cuda:0" if plans.backend(plugin_id) == "cuda" else "cpu")
                if plans.family(plugin_id) == "fish":
                    self.assertEqual(result["actions"][0]["id"], "fish_project_root")
                else:
                    self.assertEqual(result["actions"], [])
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
                self.assertEqual(plans.settings(plugin_id)["rvc.pitch_shift_semitones"]["default"], 6.0)

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
            plugin.initialize({"silero.speaker_id": "does-not-exist"})


    def test_namespaced_silero_and_rvc_settings_and_strict_validation(self):
        schema = plans.settings("silero_rvc_cuda")
        self.assertIn("silero.speaker_id", schema)
        self.assertIn("rvc.pitch_shift_semitones", schema)
        self.assertNotIn("speaker", schema)
        from neuromita_ai_sdk import validate_settings
        valid = validate_settings(schema, {"silero.speaker_id": "kseniya", "rvc.pitch_shift_semitones": 6})
        self.assertEqual(valid["rvc.pitch_shift_semitones"], 6)
        for invalid in ({"rvc.pitch_shift_semitones": True}, {"rvc.pitch_shift_semitones": float("nan")},
                        {"rvc.pitch_shift_semitones": 100}, {"volume": "1"}, {"unknown": 0}):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                validate_settings(schema, invalid)

    def test_probe_rejects_wrong_installed_numpy_version(self):
        import importlib.metadata
        original_version = importlib.metadata.version
        def fake_version(name):
            if name == "numpy":
                return "2.0.0"
            return original_version(name)
        with tempfile.TemporaryDirectory() as root, patch.dict("os.environ", {"NEUROMITA_AI_MODELS_DIR": root}), patch("importlib.metadata.version", side_effect=fake_version):
            result = plans.probe("f5_tts_ru")
            self.assertFalse(result["installed"])
            self.assertEqual(result["wrong_versions"][0]["name"], "numpy")

    def test_assets_use_explicit_selected_revision(self):
        import json
        with tempfile.TemporaryDirectory() as root, patch.dict("os.environ", {
            "NEUROMITA_AI_MODELS_DIR": root,
            "NEUROMITA_AI_MODEL_REVISIONS": json.dumps({"silero_rvc_cuda": "rev1"}),
        }):
            self.assertEqual(plans.asset_root("silero_rvc_cuda"), Path(root) / "silero_rvc_cuda/rev1")
            self.assertEqual(plans.asset_root("f5_tts_ru"), Path(root) / "f5_tts_ru")
        with patch.dict("os.environ", {"NEUROMITA_AI_MODEL_REVISIONS": '{"f5_tts_ru":"../escaped"}'}, clear=False):
            with self.assertRaises(ValueError):
                plans.asset_root("f5_tts_ru")

    def test_pcm16_volume_processing(self):
        import io
        import struct
        import wave
        from neuromita_voice.runtime import _scale_wave
        output = io.BytesIO()
        with wave.open(output, "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(16000)
            wav.writeframes(struct.pack("<3h", 1000, -1000, 24000))
        with wave.open(io.BytesIO(_scale_wave(output.getvalue(), 2)), "rb") as wav:
            self.assertEqual(struct.unpack("<3h", wav.readframes(3)), (2000, -2000, 32767))
        for volume in (-1, 3, True, float("nan")):
            with self.assertRaises(ValueError):
                _scale_wave(output.getvalue(), volume)


if __name__ == "__main__":
    unittest.main()
