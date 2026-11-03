from neuromita_voice.plans import plan, probe, settings


class InstallContract:
    def install_plan(self):
        return plan("fish_speech")

    def probe_install(self):
        return probe("fish_speech")

    def settings_schema(self):
        return settings("fish_speech")
