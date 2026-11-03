from neuromita_asr.gigaam import GigaAmPlugin


class Plugin(GigaAmPlugin):
    def __init__(self):
        super().__init__("gigaam_onnx")
