from fastapi import FastAPI
from fastapi.testclient import TestClient
from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker
from sqlalchemy.pool import StaticPool

from backend.database import Base, get_db
from backend.routes.captures import router as captures_router
from backend.routes.providers import router
from backend.routes.settings import router as settings_router


def build_client():
    engine = create_engine(
        "sqlite://",
        connect_args={"check_same_thread": False},
        poolclass=StaticPool,
    )
    testing_session = sessionmaker(bind=engine, autocommit=False, autoflush=False)
    Base.metadata.create_all(bind=engine)
    app = FastAPI()
    app.include_router(router)
    app.include_router(captures_router)
    app.include_router(settings_router)

    def override_db():
        db = testing_session()
        try:
            yield db
        finally:
            db.close()

    app.dependency_overrides[get_db] = override_db
    return TestClient(app)


def test_provider_response_never_returns_key_and_null_clears(monkeypatch):
    monkeypatch.delenv("GEMINI_API_KEY", raising=False)
    client = build_client()

    response = client.put("/providers", json={"gemini_api_key": "secret-key"})
    assert response.status_code == 200
    payload = response.json()
    assert payload["gemini_key_present"] is True
    assert payload["gemini_key_source"] == "stored"
    assert payload["gemini_key_hint"] == "-key"
    assert "gemini_api_key" not in payload
    assert "secret-key" not in response.text

    cleared = client.put("/providers", json={"gemini_api_key": None})
    assert cleared.status_code == 200
    assert cleared.json()["gemini_key_present"] is False


def test_environment_key_overrides_stored_key(monkeypatch):
    client = build_client()
    client.put("/providers", json={"gemini_api_key": "stored-key"})
    monkeypatch.setenv("GEMINI_API_KEY", "environment-key")

    payload = client.get("/providers").json()

    assert payload["gemini_key_source"] == "env"
    assert payload["gemini_key_hint"] == "-key"


def test_smart_mode_forces_incompatible_toggles_off(monkeypatch):
    monkeypatch.delenv("GEMINI_API_KEY", raising=False)
    client = build_client()

    response = client.put(
        "/providers",
        json={
            "gemini_stt_mode": "smart",
            "gemini_stt_diarization": True,
            "gemini_stt_timestamps": True,
        },
    )

    assert response.status_code == 200
    payload = response.json()
    assert payload["gemini_stt_mode"] == "smart"
    assert payload["gemini_stt_diarization"] is False
    assert payload["gemini_stt_timestamps"] is False


def test_enabling_diarization_forces_verbatim(monkeypatch):
    monkeypatch.delenv("GEMINI_API_KEY", raising=False)
    client = build_client()
    client.put("/providers", json={"gemini_stt_mode": "smart"})

    payload = client.put("/providers", json={"gemini_stt_diarization": True}).json()

    assert payload["gemini_stt_mode"] == "verbatim"
    assert payload["gemini_stt_diarization"] is True


def test_gemini_smart_readiness_does_not_require_local_llm(monkeypatch):
    monkeypatch.delenv("GEMINI_API_KEY", raising=False)
    client = build_client()
    client.put("/providers", json={"gemini_api_key": "secret-key", "gemini_stt_mode": "smart"})
    client.put("/settings/captures", json={"stt_engine": "gemini"})
    payload = client.get("/capture/readiness").json()

    assert payload["stt"]["ready"] is True
    assert payload["stt"]["size"] == "api"
    assert payload["llm"] is None


def test_gemini_verbatim_readiness_keeps_local_llm_gate(monkeypatch):
    monkeypatch.delenv("GEMINI_API_KEY", raising=False)
    client = build_client()
    client.put("/providers", json={"gemini_api_key": "secret-key", "gemini_stt_mode": "verbatim"})
    client.put("/settings/captures", json={"stt_engine": "gemini"})
    monkeypatch.setattr("backend.routes.captures.is_model_cached", lambda _repo: False)

    payload = client.get("/capture/readiness").json()

    assert payload["stt"]["ready"] is True
    assert payload["llm"] is not None
    assert payload["llm"]["ready"] is False
