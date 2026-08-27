"""STT service — delegates to the selected backend abstraction."""

from ..backends import STTBackend, get_stt_backend_for_engine, unload_stt_backends


def get_stt_model(engine: str = "whisper") -> STTBackend:
    """Return the singleton backend for an STT engine."""
    return get_stt_backend_for_engine(engine)


def unload_stt_models() -> None:
    """Unload every instantiated STT backend."""
    unload_stt_backends()
