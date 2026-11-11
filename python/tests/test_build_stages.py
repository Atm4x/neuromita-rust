import sys
import unittest
from unittest.mock import patch

from neuromita_ai_sdk import BuildStage, InstallPlan
from neuromita_voice import plans


class BuildStageContractTests(unittest.TestCase):
    def test_fish_cuda_linux_has_jit_but_no_windows_distribution(self):
        with patch.object(sys, "platform", "linux"):
            plan = plans.plan("fish_speech_compiled")
        self.assertEqual([entry.id for entry in plan.build_stages], ["fish_triton_jit"])
        self.assertEqual(plan.build_stages[0].kind, "runtime_jit")
        self.assertNotIn("triton-windows<3.4", plan.requirements)
        self.assertEqual(plan.to_wire()["build_stages"][0]["platform"], "linux")

    def test_fish_cuda_windows_declares_wheel_and_jit_separately(self):
        with patch.object(sys, "platform", "win32"):
            plan = plans.plan("fish_speech_compiled")
        self.assertEqual(
            [(stage.id, stage.kind, stage.platform) for stage in plan.build_stages],
            [
                ("fish_triton_wheel", "python_wheel", "windows"),
                ("fish_triton_jit", "runtime_jit", "windows"),
            ],
        )
        self.assertEqual(plan.build_stages[0].requirement, "triton-windows<3.4")
        self.assertIn("triton-windows<3.4", plan.requirements)

    def test_fish_standard_cuda_selection_has_same_windows_toolchain_requirements(self):
        with patch.object(sys, "platform", "win32"):
            plan = plans.plan("fish_speech", {"device": "cuda:0"})
        self.assertEqual(plan.supported_backends, ("torch-cuda",))
        self.assertEqual([stage.kind for stage in plan.build_stages], ["python_wheel", "runtime_jit"])
        self.assertIn("triton-windows<3.4", plan.requirements)

    def test_cpu_and_other_backends_have_no_triton_build_stage(self):
        for plugin_id in ("fish_speech", "f5_tts_en", "edge_tts_rvc_onnx"):
            with self.subTest(plugin=plugin_id):
                self.assertEqual(plans.plan(plugin_id).build_stages, ())

    def test_wire_is_declarative_and_does_not_create_build_artifacts(self):
        stage = BuildStage("fixture", "source_build", "torch-cuda", "linux")
        wire = InstallPlan(build_stages=(stage,)).to_wire()
        self.assertEqual(wire["build_stages"], [{
            "id": "fixture", "kind": "source_build", "backend": "torch-cuda",
            "platform": "linux", "requirement": None,
        }])


if __name__ == "__main__":
    unittest.main()
