from neuromita_voice.plans import plan, probe, settings


class InstallContract:
    def install_plan(self):
        return plan("silero_rvc_onnx")

    def probe_install(self):
        return probe("silero_rvc_onnx")

    def settings_schema(self):
        return settings("silero_rvc_onnx")
