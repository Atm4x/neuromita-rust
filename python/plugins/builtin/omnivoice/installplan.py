from neuromita_voice.plans import plan, probe, settings


class InstallContract:
    def install_plan(self):
        return plan("omnivoice")

    def probe_install(self):
        return probe("omnivoice")

    def settings_schema(self):
        return settings("omnivoice")
