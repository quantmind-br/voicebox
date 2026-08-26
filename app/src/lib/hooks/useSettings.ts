import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { apiClient } from '@/lib/api/client';
import type {
  CaptureSettings,
  CaptureSettingsUpdate,
  GenerationSettings,
  GenerationSettingsUpdate,
} from '@/lib/api/types';

const CAPTURE_SETTINGS_KEY = ['settings', 'captures'] as const;
const GENERATION_SETTINGS_KEY = ['settings', 'generation'] as const;

/**
 * Hook for capture/refine defaults. Reads from the server and writes partial
 * updates with optimistic cache mutation so toggles stay snappy while the
 * PUT round-trip settles.
 */
export function useCaptureSettings() {
  const queryClient = useQueryClient();

  const query = useQuery({
    queryKey: CAPTURE_SETTINGS_KEY,
    queryFn: () => apiClient.getCaptureSettings(),
    staleTime: Infinity,
    // Keep trying across a backend cold start. These two options are load
    // bearing together: `staleTime: Infinity` means a query that fails has no
    // second chance of its own, and the app's global `retry: 1` gives up
    // about a second in — but the bundled sidecar unpacks a multi-gigabyte
    // archive and loads models before it binds the port, which routinely
    // takes far longer than that.
    //
    // Losing this particular query is not cosmetic: useChordSync bails out
    // when settings are undefined, so the dictation hotkey silently never
    // arms for the rest of the session. It looked intermittent because
    // whether the backend won the race varied run to run, and opening the
    // Captures page remounted the query and appeared to "fix" it.
    retry: (failureCount) => failureCount < 12,
    retryDelay: (attempt) => Math.min(1000 * 2 ** attempt, 10_000),
  });

  const mutation = useMutation({
    mutationFn: (patch: CaptureSettingsUpdate) => apiClient.updateCaptureSettings(patch),
    onMutate: async (patch) => {
      await queryClient.cancelQueries({ queryKey: CAPTURE_SETTINGS_KEY });
      const previous = queryClient.getQueryData<CaptureSettings>(CAPTURE_SETTINGS_KEY);
      if (previous) {
        queryClient.setQueryData<CaptureSettings>(CAPTURE_SETTINGS_KEY, {
          ...previous,
          ...patch,
        });
      }
      return { previous };
    },
    onError: (_err, _patch, ctx) => {
      if (ctx?.previous) {
        queryClient.setQueryData(CAPTURE_SETTINGS_KEY, ctx.previous);
      }
    },
    onSettled: (data, _err, patch) => {
      if (data) queryClient.setQueryData(CAPTURE_SETTINGS_KEY, data);
      // /capture/readiness resolves stt_model / llm_model live on each
      // call, but its cached response keeps serving the previous
      // model's state until the next 5 s poll. Invalidate on model
      // swaps so the readiness checklist re-checks immediately.
      if (patch.stt_model !== undefined || patch.llm_model !== undefined) {
        queryClient.invalidateQueries({ queryKey: ['capture-readiness'] });
      }
    },
  });

  return {
    settings: query.data,
    isLoading: query.isLoading,
    update: mutation.mutate,
  };
}

/**
 * Hook for long-form TTS generation defaults. Same optimistic pattern as
 * ``useCaptureSettings``.
 */
export function useGenerationSettings() {
  const queryClient = useQueryClient();

  const query = useQuery({
    queryKey: GENERATION_SETTINGS_KEY,
    queryFn: () => apiClient.getGenerationSettings(),
    staleTime: Infinity,
  });

  const mutation = useMutation({
    mutationFn: (patch: GenerationSettingsUpdate) =>
      apiClient.updateGenerationSettings(patch),
    onMutate: async (patch) => {
      await queryClient.cancelQueries({ queryKey: GENERATION_SETTINGS_KEY });
      const previous = queryClient.getQueryData<GenerationSettings>(GENERATION_SETTINGS_KEY);
      if (previous) {
        queryClient.setQueryData<GenerationSettings>(GENERATION_SETTINGS_KEY, {
          ...previous,
          ...patch,
        });
      }
      return { previous };
    },
    onError: (_err, _patch, ctx) => {
      if (ctx?.previous) {
        queryClient.setQueryData(GENERATION_SETTINGS_KEY, ctx.previous);
      }
    },
    onSettled: (data) => {
      if (data) queryClient.setQueryData(GENERATION_SETTINGS_KEY, data);
    },
  });

  return {
    settings: query.data,
    isLoading: query.isLoading,
    update: mutation.mutate,
  };
}
