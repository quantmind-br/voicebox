"""Gemini API text-to-speech backend."""

from typing import Any

import numpy as np

from ..utils.gemini_api import create_generation, extract_generation_audio

GEMINI_TTS_MODELS = [
    "gemini-3.1-flash-tts-preview",
    "gemini-2.5-flash-preview-tts",
    "gemini-2.5-pro-preview-tts",
]
GEMINI_TTS_DEFAULT_MODEL = GEMINI_TTS_MODELS[0]
GEMINI_TTS_DEFAULT_VOICE = "Kore"
GEMINI_TTS_SAMPLE_RATE = 24000

GEMINI_TTS_VOICES: list[tuple[str, str, str, str, str]] = [
    ("Zephyr", "Zephyr", "neutral", "auto", "Bright"),
    ("Puck", "Puck", "neutral", "auto", "Upbeat"),
    ("Charon", "Charon", "neutral", "auto", "Informative"),
    ("Kore", "Kore", "neutral", "auto", "Firm"),
    ("Fenrir", "Fenrir", "neutral", "auto", "Excitable"),
    ("Leda", "Leda", "neutral", "auto", "Youthful"),
    ("Orus", "Orus", "neutral", "auto", "Firm"),
    ("Aoede", "Aoede", "neutral", "auto", "Breezy"),
    ("Callirrhoe", "Callirrhoe", "neutral", "auto", "Easy-going"),
    ("Autonoe", "Autonoe", "neutral", "auto", "Bright"),
    ("Enceladus", "Enceladus", "neutral", "auto", "Breathy"),
    ("Iapetus", "Iapetus", "neutral", "auto", "Clear"),
    ("Umbriel", "Umbriel", "neutral", "auto", "Easy-going"),
    ("Algieba", "Algieba", "neutral", "auto", "Smooth"),
    ("Despina", "Despina", "neutral", "auto", "Smooth"),
    ("Erinome", "Erinome", "neutral", "auto", "Clear"),
    ("Algenib", "Algenib", "neutral", "auto", "Gravelly"),
    ("Rasalgethi", "Rasalgethi", "neutral", "auto", "Informative"),
    ("Laomedeia", "Laomedeia", "neutral", "auto", "Upbeat"),
    ("Achernar", "Achernar", "neutral", "auto", "Soft"),
    ("Alnilam", "Alnilam", "neutral", "auto", "Firm"),
    ("Schedar", "Schedar", "neutral", "auto", "Even"),
    ("Gacrux", "Gacrux", "neutral", "auto", "Mature"),
    ("Pulcherrima", "Pulcherrima", "neutral", "auto", "Forward"),
    ("Achird", "Achird", "neutral", "auto", "Friendly"),
    ("Zubenelgenubi", "Zubenelgenubi", "neutral", "auto", "Casual"),
    ("Vindemiatrix", "Vindemiatrix", "neutral", "auto", "Gentle"),
    ("Sadachbia", "Sadachbia", "neutral", "auto", "Lively"),
    ("Sadaltager", "Sadaltager", "neutral", "auto", "Knowledgeable"),
    ("Sulafat", "Sulafat", "neutral", "auto", "Warm"),
]


def _speech_config(voice_id: str) -> dict[str, dict]:
    return {"voiceConfig": {"prebuiltVoiceConfig": {"voiceName": voice_id}}}


def _sample_rate_from_mime(mime_type: str) -> int:
    """Parse the sample rate out of an ``audio/l16; rate=24000; ...`` MIME type."""
    for chunk in mime_type.split(";"):
        chunk = chunk.strip()
        if chunk.startswith("rate="):
            try:
                return int(chunk.split("=", 1)[1])
            except ValueError:
                break
    return GEMINI_TTS_SAMPLE_RATE


def _channels_from_mime(mime_type: str) -> int:
    """Parse the channel count out of an ``audio/l16; ...; channels=1`` MIME type."""
    for chunk in mime_type.split(";"):
        chunk = chunk.strip()
        if chunk.startswith("channels="):
            try:
                return int(chunk.split("=", 1)[1])
            except ValueError:
                break
    return 1


def _decode_pcm(data: bytes, *, channels: int) -> np.ndarray:
    """Decode signed 16-bit little-endian PCM into float32 in [-1, 1]."""
    frame_size = 2 * max(channels, 1)
    usable = len(data) - (len(data) % frame_size)
    audio = np.frombuffer(data[:usable], dtype="<i2").astype(np.float32) / 32768.0
    if channels > 1 and audio.size:
        audio = audio.reshape(-1, channels).mean(axis=1)
    return np.ascontiguousarray(audio, dtype=np.float32)


class GeminiTTSBackend:
    """Generate speech with Gemini's preset voices."""

    async def load_model(self, model_size: str = GEMINI_TTS_DEFAULT_MODEL) -> None:
        return None

    async def load_model_async(self, model_size: str = GEMINI_TTS_DEFAULT_MODEL) -> None:
        return None

    def unload_model(self) -> None:
        return None

    def is_loaded(self) -> bool:
        return True

    def _get_model_path(self, model_size: str) -> str:
        return model_size

    async def create_voice_prompt(
        self,
        audio_path: str,
        reference_text: str,
        use_cache: bool = True,
    ) -> tuple[dict, bool]:
        return {
            "voice_type": "preset",
            "preset_engine": "gemini",
            "preset_voice_id": GEMINI_TTS_DEFAULT_VOICE,
        }, False

    async def combine_voice_prompts(
        self,
        audio_paths: list[str],
        reference_texts: list[str],
    ) -> tuple[np.ndarray, str]:
        raise RuntimeError("Gemini preset voices do not accept reference audio")

    async def generate(
        self,
        text: str,
        voice_prompt: dict,
        language: str = "auto",
        seed: int | None = None,
        instruct: str | None = None,
        *,
        options: dict | None = None,
    ) -> tuple[np.ndarray, int]:
        """Generate speech. Gemini ignores language and seed: neither is an API field."""
        del language, seed
        options = options or {}
        api_key = str(options.get("api_key") or "").strip()
        if not api_key:
            raise RuntimeError("Configure your Gemini API key in Settings > Providers")

        model = str(options.get("model") or GEMINI_TTS_DEFAULT_MODEL)
        voice_id = str(
            voice_prompt.get("preset_voice_id") or voice_prompt.get("gemini_voice") or GEMINI_TTS_DEFAULT_VOICE
        )
        style = instruct if instruct and instruct.strip() else options.get("style_prompt")
        prompt = f"{str(style).strip()}:\n{text}" if style and str(style).strip() else text

        payload: dict[str, Any] = {
            "model": model,
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {
                "responseModalities": ["AUDIO"],
                "speechConfig": _speech_config(voice_id),
            },
        }
        response = await create_generation(payload, api_key=api_key)
        raw_audio, mime_type = extract_generation_audio(response)
        sample_rate = _sample_rate_from_mime(mime_type)
        channels = _channels_from_mime(mime_type)
        audio = _decode_pcm(raw_audio, channels=channels)
        return audio, sample_rate
