"""API provider settings and credential verification endpoints."""

from fastapi import APIRouter, Depends
from sqlalchemy.orm import Session

from .. import models
from ..database import get_db
from ..services import providers as providers_service

router = APIRouter(prefix="/providers", tags=["providers"])


@router.get("", response_model=models.ProviderSettingsResponse)
async def get_provider_settings(db: Session = Depends(get_db)):
    return providers_service.get_status(db)


@router.put("", response_model=models.ProviderSettingsResponse)
async def update_provider_settings(
    patch: models.ProviderSettingsUpdate,
    db: Session = Depends(get_db),
):
    providers_service.update_provider_settings(db, patch.model_dump(exclude_unset=True))
    return providers_service.get_status(db)


@router.post("/gemini/verify", response_model=models.ProviderVerifyResponse)
async def verify_gemini_key(db: Session = Depends(get_db)):
    ok, detail = await providers_service.verify_gemini_key(db)
    return models.ProviderVerifyResponse(ok=ok, detail=detail)
