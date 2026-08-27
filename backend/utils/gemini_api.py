"""Client for Google's Gemini API (interactions, generateContent, Files upload).

Thin async wrapper over the REST surfaces Voicebox needs. Deliberately not the
google-genai SDK: it would pull google-auth, requests, tenacity, websockets and anyio
into a PyInstaller bundle for three documented endpoints."""

import asyncio
import base64
from typing import Any

import httpx

BASE_URL = "https://generativelanguage.googleapis.com"

# Base64 inflates payloads ~33%; this keeps an inline request under the 20 MB cap.
# Anything larger goes through the Files API.
INLINE_LIMIT_BYTES = 14 * 1024 * 1024

_RETRY_STATUSES = {429, 500, 502, 503, 504}
_RETRY_BACKOFF_SECONDS = (1.0, 3.0)
_FILE_ACTIVE_POLLS = 60


class GeminiApiError(RuntimeError):
    """A Gemini API call failed. ``status`` is the HTTP code when there was one."""

    def __init__(self, message: str, *, status: int | None = None) -> None:
        super().__init__(message)
        self.status = status


def _error_message(response: httpx.Response) -> str:
    """Return Google's actionable error text when the response carries one."""
    try:
        payload = response.json()
    except ValueError:
        payload = None
    if isinstance(payload, dict):
        error = payload.get("error")
        if isinstance(error, dict) and error.get("message"):
            return str(error["message"])
    return f"HTTP {response.status_code}: {response.text[:400]}"


async def _post_json_retry(url: str, payload: dict, *, api_key: str, timeout: float, retries: int) -> dict:
    """POST JSON to a Gemini endpoint, retrying transient failures."""
    headers = {"x-goog-api-key": api_key, "Content-Type": "application/json"}
    last = "no attempt was made"
    status: int | None = None

    async with httpx.AsyncClient(timeout=timeout) as client:
        for attempt in range(retries + 1):
            try:
                response = await client.post(url, json=payload, headers=headers)
            except httpx.TransportError as exc:
                last = f"network error talking to Gemini: {exc}"
                status = None
            else:
                if response.status_code < 400:
                    try:
                        return response.json()
                    except ValueError as exc:
                        raise GeminiApiError(f"Gemini returned a non-JSON response: {response.text[:200]}") from exc
                last = _error_message(response)
                status = response.status_code
                if status not in _RETRY_STATUSES:
                    raise GeminiApiError(last, status=status)
            if attempt < retries:
                await asyncio.sleep(_RETRY_BACKOFF_SECONDS[min(attempt, 1)])

    raise GeminiApiError(last, status=status)


async def create_interaction(payload: dict, *, api_key: str, timeout: float = 300.0, retries: int = 2) -> dict:
    """POST one interaction (native-audio transcription) and return the parsed response."""
    return await _post_json_retry(
        f"{BASE_URL}/v1beta/interactions",
        payload,
        api_key=api_key,
        timeout=timeout,
        retries=retries,
    )


async def create_generation(payload: dict, *, api_key: str, timeout: float = 300.0, retries: int = 2) -> dict:
    """POST one generateContent request (TTS preview models) and return the parsed response.

    ``payload["model"]`` only builds the URL path; the request body sent to
    Google is the rest of the payload.
    """
    model = str(payload.get("model") or "").strip()
    if not model:
        raise GeminiApiError("generateContent requires a 'model' field")
    body = {key: value for key, value in payload.items() if key != "model"}
    return await _post_json_retry(
        f"{BASE_URL}/v1beta/models/{model}:generateContent",
        body,
        api_key=api_key,
        timeout=timeout,
        retries=retries,
    )


async def upload_file(
    data: bytes,
    *,
    mime_type: str,
    display_name: str,
    api_key: str,
    timeout: float = 300.0,
) -> str:
    """Upload bytes through the resumable Files API and return the active file URI."""
    headers = {"x-goog-api-key": api_key}
    async with httpx.AsyncClient(timeout=timeout) as client:
        start = await client.post(
            f"{BASE_URL}/upload/v1beta/files",
            headers={
                **headers,
                "X-Goog-Upload-Protocol": "resumable",
                "X-Goog-Upload-Command": "start",
                "X-Goog-Upload-Header-Content-Length": str(len(data)),
                "X-Goog-Upload-Header-Content-Type": mime_type,
                "Content-Type": "application/json",
            },
            json={"file": {"display_name": display_name}},
        )
        if start.status_code >= 400:
            raise GeminiApiError(_error_message(start), status=start.status_code)

        upload_url = start.headers.get("x-goog-upload-url")
        if not upload_url:
            raise GeminiApiError("Gemini did not return a resumable upload URL.")

        finalize = await client.post(
            upload_url,
            headers={
                **headers,
                "Content-Length": str(len(data)),
                "X-Goog-Upload-Offset": "0",
                "X-Goog-Upload-Command": "upload, finalize",
            },
            content=data,
        )
        if finalize.status_code >= 400:
            raise GeminiApiError(_error_message(finalize), status=finalize.status_code)

        try:
            info = (finalize.json() or {}).get("file") or {}
        except ValueError as exc:
            raise GeminiApiError("Gemini upload returned a non-JSON response.") from exc
        uri, name, state = info.get("uri"), info.get("name"), info.get("state")
        if not uri or not name:
            raise GeminiApiError("Gemini upload response carried no file URI.")

        for attempt in range(_FILE_ACTIVE_POLLS + 1):
            if state == "ACTIVE":
                return uri
            if state == "FAILED":
                raise GeminiApiError(f"Gemini failed to process the uploaded audio ({name}).")
            if attempt == _FILE_ACTIVE_POLLS:
                break
            await asyncio.sleep(1.0)
            probe = await client.get(f"{BASE_URL}/v1beta/{name}", headers=headers)
            if probe.status_code >= 400:
                raise GeminiApiError(_error_message(probe), status=probe.status_code)
            try:
                state = (probe.json() or {}).get("state")
            except ValueError as exc:
                raise GeminiApiError("Gemini file-status probe returned a non-JSON response.") from exc

    raise GeminiApiError("Gemini never finished processing the uploaded audio.")


async def list_models(api_key: str, *, timeout: float = 15.0) -> list[str]:
    """Model ids visible to this key. Costs no tokens, so it doubles as key validation."""
    async with httpx.AsyncClient(timeout=timeout) as client:
        try:
            response = await client.get(f"{BASE_URL}/v1beta/models", headers={"x-goog-api-key": api_key})
        except httpx.TransportError as exc:
            raise GeminiApiError(f"network error talking to Gemini: {exc}") from exc
    if response.status_code >= 400:
        raise GeminiApiError(_error_message(response), status=response.status_code)
    models = (response.json() or {}).get("models") or []
    return [model.get("name", "") for model in models if isinstance(model, dict)]


def _model_output_content(interaction: dict) -> list[dict]:
    out: list[dict] = []
    for step in interaction.get("steps") or []:
        if not isinstance(step, dict) or step.get("type") != "model_output":
            continue
        for content in step.get("content") or []:
            if isinstance(content, dict):
                out.append(content)
    return out


def extract_generation_audio(response: dict) -> tuple[bytes, str]:
    """Return decoded audio bytes and their MIME type from a generateContent TTS response."""
    for candidate in response.get("candidates") or []:
        if not isinstance(candidate, dict):
            continue
        content = candidate.get("content") or {}
        if not isinstance(content, dict):
            continue
        for part in content.get("parts") or []:
            if not isinstance(part, dict):
                continue
            inline = part.get("inlineData")
            if isinstance(inline, dict) and inline.get("data"):
                return (
                    base64.b64decode(inline["data"]),
                    inline.get("mimeType") or "audio/l16",
                )
    raise GeminiApiError("Gemini returned no audio. The response was truncated or blocked by a safety filter.")


def extract_output_text(interaction: dict) -> str:
    """Return concatenated text from every model-output step."""
    parts = [
        content["text"]
        for content in _model_output_content(interaction)
        if content.get("type") == "text" and content.get("text")
    ]
    if not parts:
        text = interaction.get("output_text")
        if isinstance(text, str) and text:
            return text.strip()
        raise GeminiApiError("Gemini returned no transcript. The response was truncated or blocked by a safety filter.")
    return "".join(parts).strip()


def extract_word_annotations(interaction: dict) -> list[dict[str, Any]]:
    """Return word-info annotations produced by timestamps or diarization."""
    words: list[dict[str, Any]] = []
    for content in _model_output_content(interaction):
        for annotation in content.get("annotations") or []:
            if isinstance(annotation, dict) and annotation.get("type") == "word_info":
                words.append(annotation)
    return words
