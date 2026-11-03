from neuromita_voice.plans import plan, probe, settings


class InstallContract:
    def install_plan(self):
        return plan("edge_tts_rvc_onnx")

    def probe_install(self):
        return probe("edge_tts_rvc_onnx")

    def settings_schema(self):
        return settings("edge_tts_rvc_onnx")
