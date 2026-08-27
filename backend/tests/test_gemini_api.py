import base64
from typing import ClassVar

import httpx
import pytest

from backend.utils import gemini_api


class FakeAsyncClient:
    responses: ClassVar[list[httpx.Response]] = []

    def __init__(self, *args, **kwargs):
        self.calls = []

    async def __aenter__(self):
        return self

    async def __aexit__(self, exc_type, exc, tb):
        return False

    async def post(self, url, **kwargs):
        self.calls.append(("POST", url, kwargs))
        return self.responses.pop(0)

    async def get(self, url, **kwargs):
        self.calls.append(("GET", url, kwargs))
        return self.responses.pop(0)


@pytest.mark.asyncio
async def test_create_interaction_retries_429_then_succeeds(monkeypatch):
    request = httpx.Request("POST", "https://example.test")
    FakeAsyncClient.responses = [
        httpx.Response(429, json={"error": {"message": "slow down"}}, request=request),
        httpx.Response(200, json={"output_text": "ok"}, request=request),
    ]
    monkeypatch.setattr(gemini_api.httpx, "AsyncClient", FakeAsyncClient)

    async def no_sleep(_seconds):
        return None

    monkeypatch.setattr(gemini_api.asyncio, "sleep", no_sleep)

    result = await gemini_api.create_interaction({"model": "x"}, api_key="key")

    assert result == {"output_text": "ok"}


@pytest.mark.asyncio
async def test_create_interaction_preserves_google_error(monkeypatch):
    request = httpx.Request("POST", "https://example.test")
    FakeAsyncClient.responses = [httpx.Response(400, json={"error": {"message": "invalid API key"}}, request=request)]
    monkeypatch.setattr(gemini_api.httpx, "AsyncClient", FakeAsyncClient)

    with pytest.raises(gemini_api.GeminiApiError, match="invalid API key") as exc:
        await gemini_api.create_interaction({"model": "x"}, api_key="bad")

    assert exc.value.status == 400


@pytest.mark.asyncio
async def test_create_generation_strips_model_and_targets_generate_content(monkeypatch):
    captured = {}

    async def fake_post(url, payload, *, api_key, timeout, retries):
        captured["url"] = url
        captured["payload"] = payload
        captured["api_key"] = api_key
        return {"candidates": []}

    monkeypatch.setattr(gemini_api, "_post_json_retry", fake_post)

    result = await gemini_api.create_generation(
        {"model": "gemini-3.1-flash-tts-preview", "contents": [{"parts": [{"text": "hi"}]}]},
        api_key="key",
    )

    assert result == {"candidates": []}
    assert captured["url"] == f"{gemini_api.BASE_URL}/v1beta/models/gemini-3.1-flash-tts-preview:generateContent"
    assert captured["api_key"] == "key"
    assert "model" not in captured["payload"]
    assert captured["payload"] == {"contents": [{"parts": [{"text": "hi"}]}]}


@pytest.mark.asyncio
async def test_create_generation_requires_model():
    with pytest.raises(gemini_api.GeminiApiError, match="requires a 'model'"):
        await gemini_api.create_generation({"contents": []}, api_key="key")


def test_extract_generation_audio_from_candidates_inline_data():
    raw = b"\x00\x01\x02\x03"
    response = {
        "candidates": [
            {
                "content": {
                    "parts": [
                        {
                            "inlineData": {
                                "mimeType": "audio/l16; rate=24000; channels=1",
                                "data": base64.b64encode(raw).decode(),
                            }
                        }
                    ]
                }
            }
        ]
    }

    assert gemini_api.extract_generation_audio(response) == (raw, "audio/l16; rate=24000; channels=1")


def test_extract_generation_audio_raises_when_missing():
    with pytest.raises(gemini_api.GeminiApiError, match="returned no audio"):
        gemini_api.extract_generation_audio({"candidates": [{"content": {"parts": [{"text": "hi"}]}}]})


def test_inline_limit_boundary_is_strictly_greater():
    assert len(b"x" * gemini_api.INLINE_LIMIT_BYTES) <= gemini_api.INLINE_LIMIT_BYTES
    assert len(b"x" * (gemini_api.INLINE_LIMIT_BYTES + 1)) > gemini_api.INLINE_LIMIT_BYTES
