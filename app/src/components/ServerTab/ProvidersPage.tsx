import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { Textarea } from '@/components/ui/textarea';
import { Toggle } from '@/components/ui/toggle';
import { useToast } from '@/components/ui/use-toast';
import { useProviderSettings } from '@/lib/hooks/useProviderSettings';
import { SettingRow, SettingSection } from './SettingRow';

const TTS_MODELS = [
  'gemini-3.1-flash-tts-preview',
  'gemini-2.5-flash-preview-tts',
  'gemini-2.5-pro-preview-tts',
] as const;

function parseCommaList(value: string): string[] {
  return value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
}

export function ProvidersPage() {
  const { t } = useTranslation();
  const { toast } = useToast();
  const { data, isLoading, update, updateAsync, isUpdating, verify, isVerifying } = useProviderSettings();
  const [apiKey, setApiKey] = useState('');
  const [sttModel, setSttModel] = useState('');
  const [languageCodes, setLanguageCodes] = useState('');
  const [vocabulary, setVocabulary] = useState('');
  const [stylePrompt, setStylePrompt] = useState('');
  const draftsInitialized = useRef(false);

  useEffect(() => {
    if (!data || draftsInitialized.current) return;
    draftsInitialized.current = true;
    setSttModel(data.gemini_stt_model);
    setLanguageCodes(data.gemini_stt_language_codes.join(', '));
    setVocabulary(data.gemini_stt_custom_vocabulary.join('\n'));
    setStylePrompt(data.gemini_tts_style_prompt ?? '');
  }, [data]);

  if (isLoading || !data) return null;

  const envManaged = data.gemini_key_source === 'env';
  const smartMode = data.gemini_stt_mode === 'smart';
  const keyStatus = data.gemini_key_present
    ? t('settings.providers.key.stored', { hint: data.gemini_key_hint })
    : t('settings.providers.key.missing');

  const saveKey = async () => {
    const trimmed = apiKey.trim();
    if (!trimmed) return;
    try {
      await updateAsync({ gemini_api_key: trimmed });
      setApiKey('');
    } catch (error) {
      toast({
        title: t('settings.providers.key.saveFailed'),
        description: error instanceof Error ? error.message : undefined,
        variant: 'destructive',
      });
    }
  };

  const testKey = async () => {
    try {
      const result = await verify();
      toast({
        title: result.ok
          ? t('settings.providers.key.testSuccess')
          : t('settings.providers.key.testFailed'),
        description: result.detail,
        variant: result.ok ? 'default' : 'destructive',
      });
    } catch (error) {
      toast({
        title: t('settings.providers.key.testFailed'),
        description: error instanceof Error ? error.message : undefined,
        variant: 'destructive',
      });
    }
  };

  return (
    <div className="max-w-2xl space-y-10">
      <SettingSection
        title={t('settings.providers.gemini.title')}
        description={t('settings.providers.gemini.description')}
      >
        <SettingRow
          title={t('settings.providers.key.title')}
          description={envManaged ? t('settings.providers.key.envManaged') : keyStatus}
        >
          <div className="flex gap-2">
            <Input
              type="password"
              value={apiKey}
              disabled={envManaged || isUpdating}
              placeholder={t('settings.providers.key.placeholder')}
              onChange={(event) => setApiKey(event.target.value)}
            />
            <Button disabled={envManaged || isUpdating} onClick={saveKey}>
              {t('settings.providers.key.save')}
            </Button>
            <Button
              variant="outline"
              disabled={envManaged || isUpdating || !data.gemini_key_present}
              onClick={() => update({ gemini_api_key: null })}
            >
              {t('settings.providers.key.remove')}
            </Button>
            <Button
              variant="outline"
              disabled={isVerifying || !data.gemini_key_present}
              onClick={testKey}
            >
              {t('settings.providers.key.test')}
            </Button>
          </div>
        </SettingRow>
      </SettingSection>

      <SettingSection
        title={t('settings.providers.transcription.title')}
        description={t('settings.providers.transcription.description')}
      >
        <SettingRow title={t('settings.providers.transcription.model')}>
          <Input
            value={sttModel}
            onChange={(event) => setSttModel(event.target.value)}
            onBlur={() => update({ gemini_stt_model: sttModel.trim() })}
          />
        </SettingRow>
        <SettingRow
          title={t('settings.providers.transcription.mode.title')}
          description={t(`settings.providers.transcription.mode.${data.gemini_stt_mode}`)}
          action={
            <Select
              value={data.gemini_stt_mode}
              onValueChange={(value: 'verbatim' | 'smart') => update({ gemini_stt_mode: value })}
            >
              <SelectTrigger className="w-40">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="verbatim">
                  {t('settings.providers.transcription.mode.verbatimLabel')}
                </SelectItem>
                <SelectItem value="smart">
                  {t('settings.providers.transcription.mode.smartLabel')}
                </SelectItem>
              </SelectContent>
            </Select>
          }
        />
        <SettingRow
          title={t('settings.providers.transcription.diarization')}
          action={
            <Toggle
              checked={data.gemini_stt_diarization}
              disabled={smartMode}
              onCheckedChange={(value) => update({ gemini_stt_diarization: value })}
            />
          }
        />
        <SettingRow
          title={t('settings.providers.transcription.timestamps')}
          action={
            <Toggle
              checked={data.gemini_stt_timestamps}
              disabled={smartMode}
              onCheckedChange={(value) => update({ gemini_stt_timestamps: value })}
            />
          }
        />
        <SettingRow title={t('settings.providers.transcription.languages')}>
          <Input
            value={languageCodes}
            placeholder="en-US, pt-BR"
            onChange={(event) => setLanguageCodes(event.target.value)}
            onBlur={() => update({ gemini_stt_language_codes: parseCommaList(languageCodes) })}
          />
        </SettingRow>
        <SettingRow title={t('settings.providers.transcription.vocabulary')}>
          <Textarea
            value={vocabulary}
            placeholder={t('settings.providers.transcription.vocabularyPlaceholder')}
            onChange={(event) => setVocabulary(event.target.value)}
            onBlur={() =>
              update({
                gemini_stt_custom_vocabulary: vocabulary
                  .split('\n')
                  .map((item) => item.trim())
                  .filter(Boolean),
              })
            }
          />
        </SettingRow>
      </SettingSection>

      <SettingSection
        title={t('settings.providers.speech.title')}
        description={t('settings.providers.speech.description')}
      >
        <SettingRow
          title={t('settings.providers.speech.model')}
          action={
            <Select
              value={data.gemini_tts_model}
              onValueChange={(value) => update({ gemini_tts_model: value })}
            >
              <SelectTrigger className="w-72">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {TTS_MODELS.map((model) => (
                  <SelectItem key={model} value={model}>
                    {model}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          }
        />
        <SettingRow title={t('settings.providers.speech.style')}>
          <Textarea
            value={stylePrompt}
            placeholder={t('settings.providers.speech.stylePlaceholder')}
            onChange={(event) => setStylePrompt(event.target.value)}
            onBlur={() => update({ gemini_tts_style_prompt: stylePrompt.trim() || null })}
          />
        </SettingRow>
      </SettingSection>

      <p className="text-sm text-muted-foreground">{t('settings.providers.privacy')}</p>
    </div>
  );
}
