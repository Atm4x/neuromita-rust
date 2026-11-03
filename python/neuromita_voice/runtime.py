"""Concrete TTS implementations called by the Python AI plugin host."""

from __future__ import annotations

import base64
import importlib
import io
from pathlib import Path
import tempfile
import wave

from .plans import VARIANTS, asset_root, family, plan, probe, settings


class VoicePlugin:
    def __init__(self, plugin_id: str):
        if plugin_id not in VARIANTS:
            raise ValueError(f"unknown TTS implementation: {plugin_id}")
        self.plugin_id = plugin_id
        self.options = {}
        self.engine = None
        self.rvc = None
        self.initialized = False

    def describe(self):
        return {"id": self.plugin_id, "kind": "tts", "family": family(self.plugin_id),
                "variant": self.plugin_id}

    def install_plan(self):
        return plan(self.plugin_id)

    def probe_install(self):
        return probe(self.plugin_id)

    def settings_schema(self):
        return settings(self.plugin_id)

    def initialize(self, values):
        schema = self.settings_schema()
        unknown = set(values) - set(schema)
        if unknown:
            raise ValueError(f"unknown TTS settings: {sorted(unknown)}")
        self.options = {}
        for key, config in schema.items():
            value = values.get(key, config["default"])
            if "choices" in config and value not in config["choices"]:
                raise ValueError(f"invalid {key}: {value}")
            if config["type"] in ("number", "integer"):
                if isinstance(value, bool) or not isinstance(value, (int, float)):
                    raise ValueError(f"invalid number for {key}")
                if "min" in config and value < config["min"]:
                    raise ValueError(f"{key} below minimum")
                if "max" in config and value > config["max"]:
                    raise ValueError(f"{key} above maximum")
            self.options[key] = value
        state = self.probe_install()
        if not state["installed"]:
            raise RuntimeError(f"TTS dependencies/assets missing: {state}")
        engine, mode, cross, rvc = VARIANTS[self.plugin_id]
        root = asset_root(self.plugin_id)
        try:
            if engine in ("edge", "silero") or rvc:
                self.rvc = self._init_rvc(root, mode)
            if engine == "silero":
                from silero import silero_tts
                language = self.options["language"]
                model_id = "v3_en" if language == "en" else "v5_ru"
                self.engine, _ = silero_tts(language=language, speaker=model_id)
                self.engine.to("cpu" if mode == "onnx" else self.options["device"])
                speakers = getattr(self.engine, "speakers", None)
                if speakers is not None and self.options["speaker"] not in speakers:
                    raise ValueError("Silero speaker is not present in the loaded model")
            elif engine == "f5":
                from .pipelines.f5_pipeline import F5TTSPipeline
                if cross:
                    from .pipelines.clf5_pipeline import CrossLingualF5TTSPipeline
                    klass = CrossLingualF5TTSPipeline
                else:
                    klass = F5TTSPipeline
                folder = root / ("en" if cross else "ru")
                options = {"model": "F5TTS_v1_Base", "ckpt_file": str(folder / "model.safetensors"),
                           "vocab_file": str(folder / "vocab.txt"), "load_vocoder_from_local": True,
                           "vocoder_local_path": str(root / "vocos"), "device": self.options["device"]}
                if cross:
                    options["speaking_rate_ckpt_file"] = str(folder / "speaking_rate.safetensors")
                self.engine = klass(**options)
            elif engine == "fish":
                import fish_speech_lib
                from fish_speech_lib.inference import FishSpeech
                if not (Path(fish_speech_lib.__file__).parent / ".project-root").is_file():
                    raise RuntimeError("fish_speech_lib .project-root marker missing: installer hook required")
                self.engine = FishSpeech(
                    device=self.options["device"], half=False,
                    compile_model=mode == "cuda",
                    llama_checkpoint_path=str(root),
                    decoder_checkpoint_path=str(root / "firefly-gan-vq-fsq-8x1024-21hz-generator.pth"),
                )
            elif engine == "omni":
                import torch
                from omnivoice import OmniVoice
                device = self.options["device"]
                if device.startswith("cuda") and not torch.cuda.is_available():
                    raise RuntimeError("CUDA requested but not available")
                self.engine = OmniVoice.from_pretrained(
                    str(root), device_map=device,
                    dtype=torch.float16 if device.startswith("cuda") else torch.float32,
                )
            else:
                self.engine = self.rvc
        except BaseException:
            self.unload()
            raise
        self.initialized = True
        return {"model_ready": True, "plugin_id": self.plugin_id}

    def _init_rvc(self, root, mode):
        candidates = ("tts_with_rvc", "tts_with_rvc_onnx") if mode == "onnx" else ("tts_with_rvc",)
        module = None
        for name in candidates:
            try:
                module = importlib.import_module(name)
                break
            except ImportError:
                continue
        if module is None:
            raise ImportError(f"RVC runtime missing: {candidates}")
        ext = "onnx" if mode == "onnx" else "pth"
        device = self.options["device"]
        if mode == "onnx" and device.startswith("cuda"):
            raise ValueError("ONNX RVC requires a supported ONNX device")
        runtime = module.TTS_RVC(model_path=str(root / "rvc" / f"voice.{ext}"),
                                 device=device, f0_method=self.options["f0_method"])
        index = root / "rvc" / "voice.index"
        runtime.set_index_path(str(index) if index.is_file() else "")
        return runtime

    def _rvc_convert(self, input_file):
        output = self.rvc.voiceover_file(
            input_path=str(input_file), pitch=float(self.options["pitch"]),
            index_rate=float(self.options["index_rate"]),
            protect=float(self.options["protect"]),
            f0method=self.options["f0_method"],
        )
        path = Path(output)
        if not path.is_file() or not path.stat().st_size:
            raise RuntimeError("RVC produced no audio")
        return path

    def infer(self, input_data):
        if not self.initialized:
            raise RuntimeError("TTS not initialized")
        text = input_data.get("text")
        if not isinstance(text, str) or not text.strip():
            raise ValueError("nonempty text is required")
        engine, mode, cross, rvc = VARIANTS[self.plugin_id]
        with tempfile.TemporaryDirectory(prefix="neuromita-tts-") as tmp:
            path = Path(tmp) / "audio.wav"
            if engine == "silero":
                import soundfile as sf
                audio = self.engine.apply_tts(
                    ssml_text=text, speaker=self.options["speaker"],
                    sample_rate=self.options["sample_rate"],
                    put_accent=True, put_yo=True, put_stress_homo=True, put_yo_homo=True)
                sf.write(path, audio.cpu().numpy(), self.options["sample_rate"])
            elif engine == "edge":
                self.rvc.set_voice(self.options["edge_voice"])
                output = self.rvc(text=text, pitch=float(self.options["pitch"]),
                                  index_rate=float(self.options["index_rate"]),
                                  protect=float(self.options["protect"]),
                                  f0method=self.options["f0_method"],
                                  tts_rate=int(self.options["tts_rate"]))
                path = Path(output)
            elif engine == "f5":
                ref_audio = input_data.get("reference_audio")
                ref_text = input_data.get("reference_text")
                if not isinstance(ref_audio, str) or not Path(ref_audio).is_file() or not isinstance(ref_text, str) or not ref_text:
                    raise ValueError("F5-TTS requires an explicit reference_audio and reference_text")
                self.engine.generate(text_to_generate=text, output_path=str(path),
                                     ref_audio=ref_audio, ref_text=ref_text,
                                     speed=self.options["speed"], nfe_step=self.options["nfe_step"],
                                     seed=self.options["seed"], remove_silence=True)
            elif engine == "fish":
                import soundfile as sf
                ref_audio = input_data.get("reference_audio")
                ref_text = input_data.get("reference_text", "")
                if ref_audio and not Path(ref_audio).is_file():
                    raise ValueError("Fish Speech reference audio does not exist")
                sample_rate, data = self.engine(
                    text=text, reference_audio=ref_audio, reference_audio_text=ref_text,
                    top_p=self.options["top_p"], temperature=self.options["temperature"],
                    repetition_penalty=self.options["repetition_penalty"],
                    max_new_tokens=self.options["max_new_tokens"],
                    chunk_length=self.options["chunk_length"], seed=self.options["seed"],
                    use_memory_cache=True)
                sf.write(path, data, sample_rate)
            elif engine == "omni":
                import soundfile as sf
                ref_audio = input_data.get("reference_audio")
                ref_text = input_data.get("reference_text")
                if not isinstance(ref_audio, str) or not Path(ref_audio).is_file() or not isinstance(ref_text, str) or not ref_text:
                    raise ValueError("OmniVoice requires explicit reference_audio and reference_text")
                prompt = self.engine.create_voice_clone_prompt(ref_audio=ref_audio, ref_text=ref_text)
                generated = self.engine.generate(
                    text=text, voice_clone_prompt=prompt, speed=self.options["speed"],
                    num_step=self.options["num_step"],
                    postprocess_output=self.options["postprocess_output"])
                if not generated:
                    raise RuntimeError("OmniVoice returned no audio")
                sf.write(path, generated[0], int(self.engine.sampling_rate), subtype="PCM_16")
            if rvc or engine == "silero":
                path = self._rvc_convert(path)
            if not path.is_file() or not path.stat().st_size:
                raise RuntimeError("TTS produced no audio")
            data = path.read_bytes()
            if len(data) > 10 * 1024 * 1024:
                raise RuntimeError("TTS audio exceeds IPC size limit; streaming transport required")
            if data[:4] != b"RIFF" or data[8:12] != b"WAVE":
                raise ValueError("TTS backend did not return a RIFF/WAVE file")
            return {"format": "wav", "audio_base64": base64.b64encode(data).decode("ascii")}

    def unload(self):
        self.engine = None
        self.rvc = None
        self.initialized = False
