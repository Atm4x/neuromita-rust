from neuromita_voice.plans import plan, probe, settings


class InstallContract:
    def install_plan(self):
        return plan("f5_tts_en")

    def probe_install(self):
        return probe("f5_tts_en")

    def settings_schema(self):
        return settings("f5_tts_en")
