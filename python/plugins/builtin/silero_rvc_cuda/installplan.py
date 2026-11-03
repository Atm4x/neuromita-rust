from neuromita_voice.plans import plan, probe, settings


class InstallContract:
    def install_plan(self):
        return plan("silero_rvc_cuda")

    def probe_install(self):
        return probe("silero_rvc_cuda")

    def settings_schema(self):
        return settings("silero_rvc_cuda")
