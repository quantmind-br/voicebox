/**
 * Supported languages for voice generation, per engine.
 *
 * Qwen3-TTS supports 10 languages.
 * LuxTTS is English-only.
 * Chatterbox Multilingual supports 23 languages.
 * Chatterbox Turbo is English-only.
 * Kokoro supports 8 languages.
 */

/** All languages that any engine supports. */
export const ALL_LANGUAGES = {
  ar: 'Arabic',
  da: 'Danish',
  de: 'German',
  el: 'Greek',
  en: 'English',
  es: 'Spanish',
  fi: 'Finnish',
  fr: 'French',
  he: 'Hebrew',
  hi: 'Hindi',
  it: 'Italian',
  ja: 'Japanese',
  ko: 'Korean',
  ms: 'Malay',
  nl: 'Dutch',
  no: 'Norwegian',
  pl: 'Polish',
  pt: 'Portuguese',
  ru: 'Russian',
  sv: 'Swedish',
  sw: 'Swahili',
  tr: 'Turkish',
  zh: 'Chinese',
} as const;

export type LanguageCode = keyof typeof ALL_LANGUAGES;

/**
 * Languages offerable at generation time, which is a superset of the concrete
 * ones: 'auto' leaves the model to infer from the text and the voice-clone
 * reference.
 *
 * Kept out of ALL_LANGUAGES on purpose. That list also drives the voice
 * profile forms, where the field records what language the reference sample
 * was actually recorded in — a fact about an existing file, which cannot be
 * "auto".
 */
export const GENERATION_LANGUAGES = {
  auto: 'Auto-detect',
  ...ALL_LANGUAGES,
} as const;

export type GenerationLanguageCode = keyof typeof GENERATION_LANGUAGES;

export const GENERATION_LANGUAGE_CODES = Object.keys(
  GENERATION_LANGUAGES,
) as GenerationLanguageCode[];

/** Per-engine supported language codes. */
export const ENGINE_LANGUAGES: Record<string, readonly GenerationLanguageCode[]> = {
  // 'auto' first so it reads as the default choice rather than an oddity at
  // the bottom of the list. Qwen-only: it maps to the model's own Auto path,
  // which the other engines do not have.
  qwen: ['auto', 'zh', 'en', 'ja', 'ko', 'de', 'fr', 'ru', 'pt', 'es', 'it'],
  luxtts: ['en'],
  chatterbox: [
    'ar',
    'da',
    'de',
    'el',
    'en',
    'es',
    'fi',
    'fr',
    'he',
    'hi',
    'it',
    'ja',
    'ko',
    'ms',
    'nl',
    'no',
    'pl',
    'pt',
    'ru',
    'sv',
    'sw',
    'tr',
    'zh',
  ],
  chatterbox_turbo: ['en'],
  tada: ['en', 'ar', 'zh', 'de', 'es', 'fr', 'it', 'ja', 'pl', 'pt'],
  kokoro: ['en', 'es', 'fr', 'hi', 'it', 'pt', 'ja', 'zh'],
  qwen_custom_voice: ['auto', 'zh', 'en', 'ja', 'ko', 'de', 'fr', 'ru', 'pt', 'es', 'it'],
} as const;

/** Helper: get language options for a given engine. */
export function getLanguageOptionsForEngine(engine: string) {
  const codes = ENGINE_LANGUAGES[engine] ?? ENGINE_LANGUAGES.qwen;
  return codes.map((code) => ({
    value: code,
    label: GENERATION_LANGUAGES[code],
  }));
}

// ── Backwards-compatible exports used elsewhere ──────────────────────
export const SUPPORTED_LANGUAGES = ALL_LANGUAGES;
export const LANGUAGE_CODES = Object.keys(ALL_LANGUAGES) as LanguageCode[];
export const LANGUAGE_OPTIONS = LANGUAGE_CODES.map((code) => ({
  value: code,
  label: ALL_LANGUAGES[code],
}));
