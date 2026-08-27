"""Capture (voice input) endpoints."""

import logging

from fastapi import APIRouter, Depends, File, Form, HTTPException, UploadFile
from fastapi.responses import FileResponse
from sqlalchemy.orm import Session

from .. import config, models
from ..backends import get_llm_model_configs, get_stt_model_configs
from ..backends.base import is_model_cached
from ..database import Capture as DBCapture, get_db
from ..services import captures as captures_service, providers as providers_service, settings as settings_service
from ..services.refinement import RefinementFlags

logger = logging.getLogger(__name__)

router = APIRouter()

UPLOAD_CHUNK_SIZE = 1024 * 1024  # 1 MB


@router.post("/captures", response_model=models.CaptureCreateResponse)
async def create_capture_endpoint(
    file: UploadFile = File(...),
    source: str = Form("file"),
    language: str | None = Form(None),
    stt_model: str | None = Form(None),
    engine: str | None = Form(None),
    db: Session = Depends(get_db),
):
    """Upload audio, run STT, persist the capture."""
    chunks = []
    while chunk := await file.read(UPLOAD_CHUNK_SIZE):
        chunks.append(chunk)
    audio_bytes = b"".join(chunks)

    if not audio_bytes:
        raise HTTPException(status_code=400, detail="Uploaded file is empty")

    saved = settings_service.get_capture_settings(db)
    selected_engine = engine or saved.stt_engine
    if selected_engine not in {"whisper", "gemini"}:
        raise HTTPException(status_code=400, detail=f"Unknown STT engine: {selected_engine}")
    options = providers_service.gemini_stt_options(db) if selected_engine == "gemini" else None
    if selected_engine == "gemini" and not (options or {}).get("api_key"):
        raise HTTPException(
            status_code=400,
            detail="Configure your Gemini API key in Settings > Providers",
        )
    resolved_stt = str((options or {})["model"]) if selected_engine == "gemini" else stt_model or saved.stt_model
    if language is None:
        resolved_language = None if saved.language == "auto" else saved.language
    else:
        resolved_language = None if language == "auto" else language

    try:
        capture = await captures_service.create_capture(
            audio_bytes=audio_bytes,
            filename=file.filename or "capture.wav",
            source=source,
            language=resolved_language,
            stt_model=resolved_stt,
            engine=selected_engine,
            options=options,
            db=db,
        )
    except ValueError as e:
        raise HTTPException(status_code=400, detail=str(e))
    except Exception as e:
        logger.exception("Failed to create capture")
        raise HTTPException(status_code=500, detail=str(e))

    return models.CaptureCreateResponse(
        **capture.model_dump(),
        auto_refine=bool(saved.auto_refine)
        and not (selected_engine == "gemini" and (options or {}).get("mode") == "smart"),
        allow_auto_paste=bool(saved.allow_auto_paste),
    )


@router.get("/captures", response_model=models.CaptureListResponse)
async def list_captures_endpoint(
    limit: int = 50,
    offset: int = 0,
    db: Session = Depends(get_db),
):
    if limit < 1 or limit > 200:
        raise HTTPException(status_code=400, detail="limit must be between 1 and 200")
    if offset < 0:
        raise HTTPException(status_code=400, detail="offset must be >= 0")

    items, total = captures_service.list_captures(db, limit=limit, offset=offset)
    return models.CaptureListResponse(items=items, total=total)


@router.get("/captures/{capture_id}", response_model=models.CaptureResponse)
async def get_capture_endpoint(capture_id: str, db: Session = Depends(get_db)):
    capture = captures_service.get_capture(capture_id, db)
    if not capture:
        raise HTTPException(status_code=404, detail="Capture not found")
    return capture


@router.get("/captures/{capture_id}/audio")
async def get_capture_audio_endpoint(capture_id: str, db: Session = Depends(get_db)):
    """Stream the original capture audio file."""
    row = db.query(DBCapture).filter(DBCapture.id == capture_id).first()
    if not row:
        raise HTTPException(status_code=404, detail="Capture not found")

    audio_path = config.resolve_storage_path(row.audio_path)
    if audio_path is None or not audio_path.exists():
        raise HTTPException(status_code=404, detail="Audio file not found")

    return FileResponse(
        audio_path,
        media_type="audio/wav",
        filename=f"capture_{capture_id}.wav",
    )


@router.delete("/captures/{capture_id}")
async def delete_capture_endpoint(capture_id: str, db: Session = Depends(get_db)):
    deleted = captures_service.delete_capture(capture_id, db)
    if not deleted:
        raise HTTPException(status_code=404, detail="Capture not found")
    return {"message": f"Capture {capture_id} deleted"}


@router.post("/captures/{capture_id}/refine", response_model=models.CaptureResponse)
async def refine_capture_endpoint(
    capture_id: str,
    request: models.CaptureRefineRequest,
    db: Session = Depends(get_db),
):
    saved = settings_service.get_capture_settings(db)
    if request.flags is not None:
        flags = RefinementFlags(
            smart_cleanup=request.flags.smart_cleanup,
            self_correction=request.flags.self_correction,
            preserve_technical=request.flags.preserve_technical,
        )
    else:
        flags = RefinementFlags(
            smart_cleanup=saved.smart_cleanup,
            self_correction=saved.self_correction,
            preserve_technical=saved.preserve_technical,
        )

    resolved_model = request.model_size or saved.llm_model

    try:
        capture = await captures_service.refine_capture(
            capture_id=capture_id,
            flags=flags,
            model_size=resolved_model,
            db=db,
        )
    except Exception as e:
        logger.exception("Refinement failed for capture %s", capture_id)
        raise HTTPException(status_code=500, detail=str(e))

    if not capture:
        raise HTTPException(status_code=404, detail="Capture not found")
    return capture


@router.get("/capture/readiness", response_model=models.CaptureReadinessResponse)
async def capture_readiness_endpoint(db: Session = Depends(get_db)):
    """Whether the selected STT and, when needed, refinement LLM are ready.

    Gemini Smart mode replaces local refinement, so it intentionally returns
    no LLM gate. Other modes check the on-disk cache rather than RAM state so
    readiness survives backend restarts.
    """
    saved = settings_service.get_capture_settings(db)
    gemini_options = None

    if saved.stt_engine == "gemini":
        gemini_options = providers_service.gemini_stt_options(db)
        stt_readiness = models.ModelReadiness(
            ready=bool(gemini_options.get("api_key")),
            model_name="gemini",
            display_name=f"Gemini {gemini_options['model']}",
            size="api",
            size_mb=None,
        )
    else:
        stt_cfg = next(
            (c for c in get_stt_model_configs() if c.model_size == saved.stt_model),
            None,
        )
        if stt_cfg is None:
            raise HTTPException(
                status_code=500,
                detail=f"No model config for stt={saved.stt_model}",
            )
        stt_readiness = models.ModelReadiness(
            ready=is_model_cached(stt_cfg.hf_repo_id),
            model_name=stt_cfg.model_name,
            display_name=stt_cfg.display_name,
            size=stt_cfg.model_size,
            size_mb=stt_cfg.size_mb or None,
        )

    smart_gemini = bool(gemini_options and gemini_options.get("mode") == "smart")
    llm_readiness = None
    if not smart_gemini:
        llm_cfg = next(
            (c for c in get_llm_model_configs() if c.model_size == saved.llm_model),
            None,
        )
        if llm_cfg is None:
            raise HTTPException(
                status_code=500,
                detail=f"No model config for llm={saved.llm_model}",
            )
        llm_readiness = models.ModelReadiness(
            ready=is_model_cached(llm_cfg.hf_repo_id),
            model_name=llm_cfg.model_name,
            display_name=llm_cfg.display_name,
            size=llm_cfg.model_size,
            size_mb=llm_cfg.size_mb or None,
        )

    return models.CaptureReadinessResponse(stt=stt_readiness, llm=llm_readiness)


@router.post("/captures/{capture_id}/retranscribe", response_model=models.CaptureResponse)
async def retranscribe_capture_endpoint(
    capture_id: str,
    request: models.CaptureRetranscribeRequest,
    db: Session = Depends(get_db),
):
    saved = settings_service.get_capture_settings(db)
    selected_engine = request.engine or saved.stt_engine
    if selected_engine not in {"whisper", "gemini"}:
        raise HTTPException(status_code=400, detail=f"Unknown STT engine: {selected_engine}")
    options = providers_service.gemini_stt_options(db) if selected_engine == "gemini" else None
    if selected_engine == "gemini" and not (options or {}).get("api_key"):
        raise HTTPException(
            status_code=400,
            detail="Configure your Gemini API key in Settings > Providers",
        )
    resolved_stt = str((options or {})["model"]) if selected_engine == "gemini" else request.model or saved.stt_model
    if request.language is None:
        resolved_language = None if saved.language == "auto" else saved.language
    else:
        resolved_language = request.language

    try:
        capture = await captures_service.retranscribe_capture(
            capture_id=capture_id,
            stt_model=resolved_stt,
            language=resolved_language,
            engine=selected_engine,
            options=options,
            db=db,
        )
    except FileNotFoundError as e:
        raise HTTPException(status_code=410, detail=str(e))
    except Exception as e:
        logger.exception("Retranscribe failed for capture %s", capture_id)
        raise HTTPException(status_code=500, detail=str(e))

    if not capture:
        raise HTTPException(status_code=404, detail="Capture not found")
    return capture
