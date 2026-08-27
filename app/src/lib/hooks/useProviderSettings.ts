import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { apiClient } from '@/lib/api/client';
import type { ProviderSettings, ProviderSettingsUpdate } from '@/lib/api/types';

const PROVIDER_SETTINGS_KEY = ['providers'] as const;

export function useProviderSettings() {
  const queryClient = useQueryClient();
  const query = useQuery({
    queryKey: PROVIDER_SETTINGS_KEY,
    queryFn: () => apiClient.getProviderSettings(),
    staleTime: Infinity,
  });

  const updateMutation = useMutation({
    mutationFn: (patch: ProviderSettingsUpdate) => apiClient.updateProviderSettings(patch),
    onMutate: async (patch) => {
      await queryClient.cancelQueries({ queryKey: PROVIDER_SETTINGS_KEY });
      const previous = queryClient.getQueryData<ProviderSettings>(PROVIDER_SETTINGS_KEY);
      if (previous) {
        const { gemini_api_key: _secret, ...publicPatch } = patch;
        queryClient.setQueryData<ProviderSettings>(PROVIDER_SETTINGS_KEY, {
          ...previous,
          ...publicPatch,
        });
      }
      return { previous };
    },
    onError: (_error, _patch, context) => {
      if (context?.previous) {
        queryClient.setQueryData(PROVIDER_SETTINGS_KEY, context.previous);
      }
    },
    onSettled: (data) => {
      if (data) queryClient.setQueryData(PROVIDER_SETTINGS_KEY, data);
      queryClient.invalidateQueries({ queryKey: ['capture-readiness'] });
    },
  });

  const verifyMutation = useMutation({
    mutationFn: () => apiClient.verifyGeminiKey(),
  });
  return {
    data: query.data,
    isLoading: query.isLoading,
    // Fire-and-forget: `mutate` swallows rejections and routes them through
    // `onError`, so toggle/select/blur call sites don't log unhandled
    // promise rejections when a PUT fails.
    update: updateMutation.mutate,
    // Awaitable variant for flows that need the settled result (e.g. saveKey).
    updateAsync: updateMutation.mutateAsync,
    isUpdating: updateMutation.isPending,
    verify: verifyMutation.mutateAsync,
    isVerifying: verifyMutation.isPending,
  };
}
