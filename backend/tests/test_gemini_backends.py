import io
from types import SimpleNamespace

import numpy as np
import pytest
import soundfile as sf
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend import models
from backend.backends import gemini_stt_backend, gemini_tts_backend
from backend.database import Base, VoiceProfile as DBVoiceProfile
from backend.routes import generations
from backend.services import providers as providers_service


def wav_bytes() -> bytes:
    buffer = io.BytesIO()
    sf.write(buffer, np.zeros(2400, dtype=np.float32), 24000, format="WAV")
    return buffer.getvalue()


def pcm_bytes() -> bytes:
    return np.zeros(2400, dtype="<i2").tobytes()


@pytest.mark.asyncio
async def test_tts_payload_voice_style_precedence_and_no_seed(monkeypatch):
    captured = {}

    async def fake_generation(payload, *, api_key, **_kwargs):
        captured["payload"] = payload
        captured["api_key"] = api_key
        return {}

    monkeypatch.setattr(gemini_tts_backend, "create_generation", fake_generation)
    monkeypatch.setattr(
        gemini_tts_backend,
        "extract_generation_audio",
        lambda _response: (pcm_bytes(), "audio/l16; rate=24000; channels=1"),
    )

    backend = gemini_tts_backend.GeminiTTSBackend()
    audio, sample_rate = await backend.generate(
        "Hello",
        {"preset_voice_id": "Kore"},
        seed=123,
        instruct="Speak calmly",
        options={"api_key": "key", "style_prompt": "Global style"},
    )

    payload = captured["payload"]
    assert captured["api_key"] == "key"
    assert payload["model"] == "gemini-3.1-flash-tts-preview"
    assert payload["contents"][0]["parts"][0]["text"] == "Speak calmly:\nHello"
    assert payload["generationConfig"]["responseModalities"] == ["AUDIO"]
    assert payload["generationConfig"]["speechConfig"] == {
        "voiceConfig": {"prebuiltVoiceConfig": {"voiceName": "Kore"}}
    }
    assert "seed" not in payload
    assert sample_rate == 24000
    assert audio.dtype == np.float32


@pytest.mark.asyncio
async def test_stt_smart_mode_excludes_incompatible_options(monkeypatch, tmp_path):
    audio_path = tmp_path / "clip.wav"
    audio_path.write_bytes(wav_bytes())
    captured = {}

    async def fake_interaction(payload, *, api_key, **_kwargs):
        captured["payload"] = payload
        return {"output_text": "clean transcript"}

    monkeypatch.setattr(gemini_stt_backend, "create_interaction", fake_interaction)
    backend = gemini_stt_backend.GeminiSTTBackend()
    text = await backend.transcribe(
        str(audio_path),
        "zh",
        options={
            "api_key": "key",
            "mode": "smart",
            "diarization": True,
            "timestamps": True,
        },
    )

    config = captured["payload"]["generation_config"]["transcription_config"]
    assert text == "clean transcript"
    assert config["language_codes"] == ["cmn-Hans-CN"]
    assert config["mode"] == {"type": "smart"}


@pytest.mark.asyncio
async def test_stt_verbatim_payload_and_speaker_turn_rendering(monkeypatch, tmp_path):
    audio_path = tmp_path / "clip.wav"
    audio_path.write_bytes(wav_bytes())
    captured = {}

    interaction = {
        "steps": [
            {
                "type": "model_output",
                "content": [
                    {
                        "type": "text",
                        "text": "fallback",
                        "annotations": [
                            {"type": "word_info", "text": "Hello ", "speaker": "spk_1", "start_offset": "1.250s"},
                            {"type": "word_info", "text": "there", "speaker": "spk_1", "start_offset": "1.500s"},
                            {"type": "word_info", "text": "Hi", "speaker": "spk_2", "start_offset": "62.000s"},
                        ],
                    }
                ],
            }
        ]
    }

    async def fake_interaction(payload, *, api_key, **_kwargs):
        captured["payload"] = payload
        return interaction

    monkeypatch.setattr(gemini_stt_backend, "create_interaction", fake_interaction)
    backend = gemini_stt_backend.GeminiSTTBackend()
    text = await backend.transcribe(
        str(audio_path),
        options={
            "api_key": "key",
            "mode": "verbatim",
            "diarization": True,
            "timestamps": True,
            "custom_vocabulary": ["Voicebox"],
        },
    )

    config = captured["payload"]["generation_config"]["transcription_config"]
    assert config["mode"] == {
        "type": "verbatim",
        "diarization_mode": "speaker",
        "timestamp_granularities": ["word"],
    }
    assert config["custom_vocabulary"] == ["Voicebox"]
    assert text == "[0:01] spk_1: Hello there\n[1:02] spk_2: Hi"


@pytest.mark.asyncio
async def test_queued_generation_snapshots_configured_gemini_model(monkeypatch):
    engine = create_engine(
        "sqlite://",
        connect_args={"check_same_thread": False},
        poolclass=StaticPool,
    )
    Base.metadata.create_all(bind=engine)
    db = sessionmaker(bind=engine)()
    profile = DBVoiceProfile(
        id="gemini-profile",
        name="Gemini profile",
        language="en",
        voice_type="preset",
        preset_engine="gemini",
        preset_voice_id="Kore",
        default_engine="gemini",
    )
    db.add(profile)
    db.commit()
    providers_service.update_provider_settings(
        db,
        {
            "gemini_api_key": "secret-key",
            "gemini_tts_model": "gemini-2.5-pro-preview-tts",
        },
    )
    captured = {}

    async def fake_get_profile(_profile_id, _db):
        return profile

    async def fake_create_generation(**kwargs):
        captured["history"] = kwargs
        return SimpleNamespace(id=kwargs["generation_id"])

    def fake_enqueue(_generation_id, awaitable):
        assert awaitable is None

    def fake_run_generation(**kwargs):
        captured["run"] = kwargs

    monkeypatch.setattr(generations.profiles, "get_profile", fake_get_profile)
    monkeypatch.setattr(generations.profiles, "validate_profile_engine", lambda *_args: None)
    monkeypatch.setattr(generations.history, "create_generation", fake_create_generation)
    monkeypatch.setattr(generations, "enqueue_generation", fake_enqueue)
    monkeypatch.setattr(generations, "run_generation", fake_run_generation)

    await generations.generate_speech(
        models.GenerationRequest(
            profile_id=profile.id,
            text="Hello",
            language="en",
            engine="gemini",
        ),
        db,
    )

    assert captured["history"]["model_size"] == "gemini-2.5-pro-preview-tts"
    assert captured["run"]["model_size"] == "gemini-2.5-pro-preview-tts"
    db.close()
