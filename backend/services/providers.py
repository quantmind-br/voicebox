"""API-provider credentials and defaults persisted in a singleton SQLite row."""

import json
import os
from typing import Any

from sqlalchemy.orm import Session

from ..database import ProviderSettings as DBProviderSettings
from .settings import _apply_patch

SINGLETON_ID = 1
DEFAULT_GEMINI_STT_MODEL = "gemini-3.5-transcribe"
DEFAULT_GEMINI_TTS_MODEL = "gemini-3.1-flash-tts-preview"


def _get_or_create_row(db: Session) -> DBProviderSettings:
    row = db.query(DBProviderSettings).filter(DBProviderSettings.id == SINGLETON_ID).first()
    if row is None:
        row = DBProviderSettings(id=SINGLETON_ID)
        db.add(row)
        db.commit()
        db.refresh(row)
    return row


def _decode_json_list(value: Any) -> list[str]:
    if not value:
        return []
    try:
        decoded = json.loads(value)
    except (TypeError, ValueError):
        return []
    if not isinstance(decoded, list):
        return []
    return [str(item) for item in decoded if isinstance(item, str) and item]


def _encode_json_list(value: Any) -> str:
    if not isinstance(value, list):
        return "[]"
    return json.dumps([str(item) for item in value if str(item)])


def resolve_gemini_api_key(db: Session) -> tuple[str | None, str | None]:
    env_key = os.getenv("GEMINI_API_KEY", "").strip()
    if env_key:
        return env_key, "env"
    row = _get_or_create_row(db)
    stored_key = (row.gemini_api_key or "").strip()
    return (stored_key, "stored") if stored_key else (None, None)


def get_provider_settings(db: Session) -> DBProviderSettings:
    return _get_or_create_row(db)


def update_provider_settings(db: Session, patch: dict[str, Any]) -> DBProviderSettings:
    row = _get_or_create_row(db)
    normalized = dict(patch)
    for key in ("gemini_stt_language_codes", "gemini_stt_custom_vocabulary"):
        if key in normalized:
            normalized[key] = _encode_json_list(normalized[key])

    if normalized.get("gemini_stt_mode") == "smart":
        normalized["gemini_stt_diarization"] = False
        normalized["gemini_stt_timestamps"] = False
    elif normalized.get("gemini_stt_diarization") or normalized.get("gemini_stt_timestamps"):
        normalized["gemini_stt_mode"] = "verbatim"

    _apply_patch(row, normalized)
    db.commit()
    db.refresh(row)
    return row


def get_status(db: Session) -> dict[str, Any]:
    row = _get_or_create_row(db)
    api_key, source = resolve_gemini_api_key(db)
    return {
        "gemini_key_present": bool(api_key),
        "gemini_key_source": source,
        "gemini_key_hint": api_key[-4:] if api_key else None,
        "gemini_stt_model": row.gemini_stt_model,
        "gemini_stt_mode": row.gemini_stt_mode,
        "gemini_stt_diarization": row.gemini_stt_diarization,
        "gemini_stt_timestamps": row.gemini_stt_timestamps,
        "gemini_stt_language_codes": _decode_json_list(row.gemini_stt_language_codes),
        "gemini_stt_custom_vocabulary": _decode_json_list(row.gemini_stt_custom_vocabulary),
        "gemini_tts_model": row.gemini_tts_model,
        "gemini_tts_style_prompt": row.gemini_tts_style_prompt,
    }


def gemini_stt_options(db: Session) -> dict[str, Any]:
    row = _get_or_create_row(db)
    api_key, _source = resolve_gemini_api_key(db)
    return {
        "api_key": api_key,
        "model": row.gemini_stt_model,
        "mode": row.gemini_stt_mode,
        "diarization": row.gemini_stt_diarization,
        "timestamps": row.gemini_stt_timestamps,
        "language_codes": _decode_json_list(row.gemini_stt_language_codes),
        "custom_vocabulary": _decode_json_list(row.gemini_stt_custom_vocabulary),
    }


def gemini_tts_options(db: Session) -> dict[str, Any]:
    row = _get_or_create_row(db)
    api_key, _source = resolve_gemini_api_key(db)
    return {
        "api_key": api_key,
        "model": row.gemini_tts_model,
        "style_prompt": row.gemini_tts_style_prompt,
    }


async def verify_gemini_key(db: Session) -> tuple[bool, str]:
    api_key, _source = resolve_gemini_api_key(db)
    if not api_key:
        return False, "Configure your Gemini API key in Settings > Providers"
    from ..utils.gemini_api import GeminiApiError, list_models

    try:
        models = await list_models(api_key)
    except GeminiApiError as exc:
        return False, str(exc)
    return True, f"Gemini API key verified ({len(models)} models available)"
