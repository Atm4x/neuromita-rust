from neuromita_voice.plans import plan, probe, settings


class InstallContract:
    def install_plan(self):
        return plan("fish_speech_compiled")

    def probe_install(self):
        return probe("fish_speech_compiled")

    def settings_schema(self):
        return settings("fish_speech_compiled")
