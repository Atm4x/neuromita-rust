"""Explicit SDK DTO and host request contracts for model enumeration/readiness."""

import unittest
from pathlib import Path

from neuromita_ai_sdk.catalog import ModelDescriptor, InstallVerification, encode_models
from neuromita_ai_sdk.host import PluginHost


ROOT = Path(__file__).resolve().parents[1]


class ModelContractTests(unittest.TestCase):
    def test_mock_plugins_expose_typed_model_lists_and_verified_installation(self):
        host = PluginHost(ROOT / "plugins/builtin")
        for plugin_id in ("mock_tts", "mock_asr"):
            models = host.dispatch("list_models", {"plugin_id": plugin_id})
            self.assertEqual(models[0]["id"], plugin_id)
            self.assertEqual(len(models), 1)
            self.assertEqual(host.dispatch("verify_install", {"plugin_id": plugin_id}), {
                "installed": True, "verified": True, "problems": []
            })

    def test_unimplemented_verification_or_enumeration_fails_explicitly(self):
        host = PluginHost(ROOT / "plugins/builtin")
        for op in ("verify_install", "list_models"):
            with self.assertRaisesRegex(ValueError, op):
                host.dispatch(op, {"plugin_id": "silero_rvc_cuda"})

    def test_dynamic_descriptors_are_bounded_unique_and_typed(self):
        self.assertEqual(encode_models([ModelDescriptor("v1", "One", ("ru", "en"))]), [
            {"id": "v1", "label": "One", "languages": ["ru", "en"]}
        ])
        for records in (
            [ModelDescriptor("same", "One"), ModelDescriptor("same", "Two")],
            [ModelDescriptor("bad/name", "Invalid")],
            [ModelDescriptor("v1", "")],
            [ModelDescriptor("v1", "One", ("english",))],
            [dict(id="untyped", label="Untyped")],
            [ModelDescriptor(str(i), "Model") for i in range(129)],
        ):
            with self.subTest(records=str(records)[:80]), self.assertRaises(ValueError):
                encode_models(records)

    def test_verification_cannot_assert_readiness_for_absent_assets(self):
        self.assertEqual(InstallVerification(False, False, ("missing model",)).to_wire(), {
            "installed": False, "verified": False, "problems": ["missing model"]
        })
        for verification in (
            InstallVerification(False, True),
            InstallVerification(True, True, ("checksum mismatch",)),
            InstallVerification(True, False, ("",)),
            InstallVerification(1, False),
        ):
            with self.subTest(verification=verification), self.assertRaises(ValueError):
                verification.to_wire()


if __name__ == "__main__":
    unittest.main()
