"""Gemini API speech-to-text backend."""

import asyncio
import base64
import tempfile
from pathlib import Path
from typing import Any

from ..utils.audio import load_audio, save_audio
from ..utils.gemini_api import (
    INLINE_LIMIT_BYTES,
    create_interaction,
    extract_output_text,
    extract_word_annotations,
    upload_file,
)

GEMINI_STT_MODELS = ["gemini-3.5-transcribe"]

GEMINI_STT_LANGUAGE_CODES = {
    "ar": "ar-EG",
    "da": "da-DK",
    "de": "de-DE",
    "el": "el-GR",
    "en": "en-US",
    "es": "es-419",
    "fi": "fi-FI",
    "fr": "fr-FR",
    "he": "he-IL",
    "hi": "hi-IN",
    "it": "it-IT",
    "ja": "ja-JP",
    "ko": "ko-KR",
    "ms": "ms-MY",
    "nl": "nl-NL",
    "no": "nb-NO",
    "pl": "pl-PL",
    "pt": "pt-BR",
    "ru": "ru-RU",
    "sv": "sv-SE",
    "sw": "sw-KE",
    "tr": "tr-TR",
    "zh": "cmn-Hans-CN",
}

GEMINI_NATIVE_AUDIO_MIMES = {
    ".wav": "audio/wav",
    ".mp3": "audio/mp3",
    ".aiff": "audio/aiff",
    ".aac": "audio/aac",
    ".ogg": "audio/ogg",
    ".flac": "audio/flac",
}


def _seconds(offset: Any) -> float:
    if not isinstance(offset, str) or not offset.endswith("s"):
        return 0.0
    try:
        return float(offset[:-1])
    except ValueError:
        return 0.0


def _timestamp(offset: Any) -> str:
    total = max(0, int(_seconds(offset)))
    minutes, seconds = divmod(total, 60)
    return f"[{minutes}:{seconds:02d}] "


def _render_annotations(words: list[dict[str, Any]], *, diarization: bool, timestamps: bool) -> str:
    """Group word annotations into speaker or time-gap turns and render them.

    With diarization, contiguous same-speaker words merge into one turn. Without
    it, words merge until a silence gap — otherwise every word (all ``speaker=None``)
    would collapse into a single turn carrying one leading timestamp.
    """
    turn_gap_seconds = 1.0
    turns: list[tuple[str | None, Any, list[str]]] = []
    previous_start: float | None = None
    for word in words:
        text = str(word.get("text") or "")
        if not text:
            continue
        speaker = str(word.get("speaker")) if word.get("speaker") else None
        start = _seconds(word.get("start_offset"))
        if turns and (
            (diarization and turns[-1][0] == speaker)
            or (not diarization and previous_start is not None and start - previous_start <= turn_gap_seconds)
        ):
            turns[-1][2].append(text)
        else:
            turns.append((speaker, word.get("start_offset"), [text]))
        previous_start = start

    rendered: list[str] = []
    for speaker, start_offset, text_parts in turns:
        prefix = ""
        if timestamps:
            prefix += _timestamp(start_offset)
        if diarization and speaker:
            prefix += f"{speaker}: "
        rendered.append(prefix + "".join(text_parts).strip())
    return "\n".join(part for part in rendered if part.strip())


class GeminiSTTBackend:
    """Transcribe audio with Gemini's API-backed transcription model."""

    async def load_model(self, model_size: str = GEMINI_STT_MODELS[0]) -> None:
        return None

    async def load_model_async(self, model_size: str = GEMINI_STT_MODELS[0]) -> None:
        return None

    def unload_model(self) -> None:
        return None

    def is_loaded(self) -> bool:
        return True

    async def transcribe(
        self,
        audio_path: str,
        language: str | None = None,
        model_size: str | None = None,
        *,
        options: dict | None = None,
    ) -> str:
        options = options or {}
        api_key = str(options.get("api_key") or "").strip()
        if not api_key:
            raise RuntimeError("Configure your Gemini API key in Settings > Providers")

        model = model_size or str(options.get("model") or GEMINI_STT_MODELS[0])
        source_path = Path(audio_path)
        request_path = source_path
        temporary_wav: Path | None = None

        try:
            suffix = source_path.suffix.lower()
            if suffix not in GEMINI_NATIVE_AUDIO_MIMES:
                with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp:
                    temporary_wav = Path(tmp.name)
                audio, sample_rate = await asyncio.to_thread(load_audio, str(source_path))
                await asyncio.to_thread(save_audio, audio, str(temporary_wav), sample_rate)
                request_path = temporary_wav
                suffix = ".wav"

            data = await asyncio.to_thread(request_path.read_bytes)
            mime_type = GEMINI_NATIVE_AUDIO_MIMES[suffix]
            if len(data) > INLINE_LIMIT_BYTES:
                uri = await upload_file(
                    data,
                    mime_type=mime_type,
                    display_name=request_path.name,
                    api_key=api_key,
                )
                audio_input = {"type": "audio", "uri": uri, "mime_type": mime_type}
            else:
                audio_input = {
                    "type": "audio",
                    "data": base64.b64encode(data).decode("ascii"),
                    "mime_type": mime_type,
                }

            configured_languages = options.get("language_codes") or []
            if configured_languages:
                language_codes = [str(code) for code in configured_languages if str(code)]
            elif language in GEMINI_STT_LANGUAGE_CODES:
                language_codes = [GEMINI_STT_LANGUAGE_CODES[language]]
            else:
                language_codes = []

            mode_name = "smart" if options.get("mode") == "smart" else "verbatim"
            mode: dict[str, Any] = {"type": mode_name}
            diarization = bool(options.get("diarization")) and mode_name == "verbatim"
            timestamps = bool(options.get("timestamps")) and mode_name == "verbatim"
            if diarization:
                mode["diarization_mode"] = "speaker"
            if timestamps:
                mode["timestamp_granularities"] = ["word"]

            transcription_config: dict[str, Any] = {
                "language_codes": language_codes,
                "mode": mode,
            }
            vocabulary = options.get("custom_vocabulary") or []
            if vocabulary:
                transcription_config["custom_vocabulary"] = [str(term) for term in vocabulary if str(term)]

            interaction = await create_interaction(
                {
                    "model": model,
                    "input": [audio_input],
                    "generation_config": {
                        "transcription_config": transcription_config,
                    },
                },
                api_key=api_key,
            )

            if diarization or timestamps:
                words = extract_word_annotations(interaction)
                if words:
                    return _render_annotations(words, diarization=diarization, timestamps=timestamps)
            return extract_output_text(interaction)
        finally:
            if temporary_wav is not None:
                temporary_wav.unlink(missing_ok=True)
