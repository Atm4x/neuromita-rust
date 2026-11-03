"""Deterministic ASR fixture for IPC and shared-worker contract tests."""

from neuromita_ai_sdk import InstallPlan


class Plugin:
    def describe(self):
        return {"kind": "asr", "languages": ["en", "ru"]}

    def install_plan(self):
        return InstallPlan()

    def probe_install(self):
        return {"installed": True, "missing": []}

    def settings_schema(self):
        return {"language": {"type": "string", "choices": ["en", "ru"], "default": "ru"}}

    def initialize(self, settings):
        if settings.get("language", "ru") not in ("en", "ru"):
            raise ValueError("unsupported language")
        return {"model_ready": True}

    def infer(self, input_data):
        return {"text": input_data.get("mock_transcript", "")}

    def unload(self):
        return None
