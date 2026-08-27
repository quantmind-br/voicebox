export const API_ENGINES: Record<string, true> = {
  gemini: true,
};

export const PRESET_ONLY_ENGINES: Record<string, true> = {
  kokoro: true,
  qwen_custom_voice: true,
  gemini: true,
};

export const CLONING_ENGINES: Record<string, true> = {
  qwen: true,
  luxtts: true,
  chatterbox: true,
  chatterbox_turbo: true,
  tada: true,
};

export const ENGINE_DISPLAY_NAMES: Record<string, string> = {
  qwen: 'Qwen',
  qwen_custom_voice: 'Qwen CustomVoice',
  luxtts: 'LuxTTS',
  chatterbox: 'Chatterbox',
  chatterbox_turbo: 'Chatterbox Turbo',
  tada: 'TADA',
  kokoro: 'Kokoro',
  gemini: 'Gemini TTS',
};

export function isApiEngine(engine?: string | null): boolean {
  return engine != null && API_ENGINES[engine] === true;
}
