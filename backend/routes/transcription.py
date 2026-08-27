"""Transcription endpoints."""

import asyncio
import tempfile
from pathlib import Path

from fastapi import APIRouter, Depends, File, Form, HTTPException, UploadFile
from sqlalchemy.orm import Session

from .. import models
from ..database import get_db
from ..services import providers as providers_service, transcribe
from ..services.task_queue import create_background_task
from ..utils.tasks import get_task_manager

router = APIRouter()

UPLOAD_CHUNK_SIZE = 1024 * 1024
ALLOWED_AUDIO_EXTS = {".wav", ".mp3", ".m4a", ".ogg", ".flac", ".aac", ".webm", ".opus"}


@router.post("/transcribe", response_model=models.TranscriptionResponse)
async def transcribe_audio(
    file: UploadFile = File(...),
    language: str | None = Form(None),
    model: str | None = Form(None),
    engine: str | None = Form(None),
    db: Session = Depends(get_db),
):
    """Transcribe an audio file with Whisper or Gemini."""
    selected_engine = engine or "whisper"
    if selected_engine not in {"whisper", "gemini"}:
        raise HTTPException(status_code=400, detail=f"Unknown STT engine: {selected_engine}")

    uploaded_ext = Path(file.filename or "").suffix.lower()
    file_suffix = uploaded_ext if uploaded_ext in ALLOWED_AUDIO_EXTS else ".wav"
    with tempfile.NamedTemporaryFile(suffix=file_suffix, delete=False) as tmp:
        while chunk := await file.read(UPLOAD_CHUNK_SIZE):
            tmp.write(chunk)
        tmp_path = tmp.name

    stt_path = tmp_path
    try:
        from ..utils.audio import load_audio, save_audio

        audio, sample_rate = await asyncio.to_thread(load_audio, tmp_path)
        duration = len(audio) / sample_rate

        if selected_engine == "whisper" and file_suffix != ".wav":
            stt_path = f"{tmp_path}.stt.wav"
            await asyncio.to_thread(save_audio, audio, stt_path, sample_rate)

        backend = transcribe.get_stt_model(selected_engine)
        options = None
        model_size = model
        if selected_engine == "whisper":
            from ..backends import WHISPER_HF_REPOS

            model_size = model or getattr(backend, "model_size", "turbo")
            valid_sizes = list(WHISPER_HF_REPOS.keys())
            if model_size not in valid_sizes:
                raise HTTPException(
                    status_code=400,
                    detail=f"Invalid model size '{model_size}'. Must be one of: {', '.join(valid_sizes)}",
                )

            already_loaded = backend.is_loaded() and getattr(backend, "model_size", None) == model_size
            if not already_loaded and not backend._is_model_cached(model_size):
                progress_model_name = f"whisper-{model_size}"
                task_manager = get_task_manager()

                async def download_whisper_background():
                    try:
                        await backend.load_model_async(model_size)
                        task_manager.complete_download(progress_model_name)
                    except Exception as exc:
                        task_manager.error_download(progress_model_name, str(exc))

                task_manager.start_download(progress_model_name)
                create_background_task(download_whisper_background())
                raise HTTPException(
                    status_code=202,
                    detail={
                        "message": f"Whisper model {model_size} is being downloaded. Please wait and try again.",
                        "model_name": progress_model_name,
                        "downloading": True,
                    },
                )
        else:
            options = providers_service.gemini_stt_options(db)
            model_size = str(options["model"])
            if not options.get("api_key"):
                raise HTTPException(
                    status_code=400,
                    detail="Configure your Gemini API key in Settings > Providers",
                )

        text = await backend.transcribe(
            stt_path,
            language,
            model_size,
            options=options,
        )
        return models.TranscriptionResponse(text=text, duration=duration)
    except HTTPException:
        raise
    except Exception as exc:
        raise HTTPException(status_code=500, detail=str(exc)) from exc
    finally:
        Path(tmp_path).unlink(missing_ok=True)
        if stt_path != tmp_path:
            Path(stt_path).unlink(missing_ok=True)
