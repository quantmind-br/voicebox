import { useQuery } from '@tanstack/react-query';
import { useAccessibilityPermission } from '@/components/AccessibilityGate/AccessibilityGate';
import { useInputMonitoringPermission } from '@/components/InputMonitoringGate/InputMonitoringGate';
import { apiClient } from '@/lib/api/client';
import type { ModelReadiness } from '@/lib/api/types';
import { usePlatform } from '@/platform/PlatformContext';

const READINESS_POLL_INTERVAL_MS = 5_000;

export type ReadinessGate = 'stt' | 'llm' | 'input_monitoring' | 'accessibility';

export interface DictationReadiness {
  isLoading: boolean;
  canRecord: boolean;
  allReady: boolean;
  /** Subset of gates that are NOT yet satisfied — what the checklist renders. */
  missing: ReadinessGate[];
  stt: ModelReadiness | undefined;
  llm: ModelReadiness | undefined;
  inputMonitoring: boolean;
  accessibility: boolean;
  /** Platform-specific remedy when `accessibility` is false; empty otherwise. */
  accessibilityHint: string;
  refetch: () => void;
  openInputMonitoringSettings: () => Promise<void>;
  openAccessibilitySettings: () => Promise<void>;
  recheckInputMonitoring: () => Promise<boolean>;
  recheckAccessibility: () => Promise<boolean>;
}

/**
 * Single source of truth for dictation readiness.
 *
 * ``canRecord`` covers the gates that must be green before the chord can
 * start recording. ``allReady`` also includes Accessibility, which only gates
 * synthetic paste — dictation still records and lands in Captures without it.
 *
 * Gates:
 *  - stt / llm: backend ``/capture/readiness`` (``llm`` is absent for
 *    Gemini Smart mode, which performs refinement remotely; model downloads
 *    finish out-of-band, so unresolved gates are polled)
 *  - input_monitoring / accessibility: macOS TCC checks via Tauri commands
 *    (rechecked on window focus by the underlying hooks)
 *
 * Hotkey-enabled is the user's intent toggle and is intentionally *not*
 * a gate here — that's `useChordSync`'s concern.
 */
export function useDictationReadiness(): DictationReadiness {
  const platform = usePlatform();
  const isTauri = platform.metadata.isTauri;

  const {
    needsPermission: inputMonNeeds,
    recheck: recheckInputMon,
    openSettings: openInputMon,
  } = useInputMonitoringPermission();
  const {
    needsPermission: a11yNeeds,
    hint: a11yHint,
    recheck: recheckA11y,
    openSettings: openA11y,
  } = useAccessibilityPermission();

  const { data, isLoading, refetch } = useQuery({
    queryKey: ['capture-readiness'],
    queryFn: () => apiClient.getCaptureReadiness(),
    // Poll only while a model is still missing/downloading. Once both are
    // green the endpoint's answer can't change until the user swaps models
    // in settings, and that path invalidates the query explicitly from
    // useSettings. refetchOnWindowFocus stays gated to the same condition.
    refetchInterval: (query) => {
      const d = query.state.data;
      return d?.stt.ready && (d.llm?.ready ?? true) ? false : READINESS_POLL_INTERVAL_MS;
    },
    refetchOnWindowFocus: (query) => {
      const d = query.state.data;
      return !(d?.stt.ready && (d.llm?.ready ?? true));
    },
  });

  // On the web build there's no TCC layer — treat both as granted so the
  // checklist doesn't block users who can't even open System Settings.
  const inputMonitoring = isTauri ? !inputMonNeeds : true;
  const accessibility = isTauri ? !a11yNeeds : true;
  const sttReady = data?.stt.ready ?? false;
  const llmReady = data?.llm?.ready ?? true;

  const missing: ReadinessGate[] = [];
  if (!sttReady) missing.push('stt');
  if (!llmReady) missing.push('llm');
  if (!inputMonitoring) missing.push('input_monitoring');
  if (!accessibility) missing.push('accessibility');
  const canRecord = sttReady && llmReady && inputMonitoring;

  return {
    isLoading,
    canRecord,
    allReady: missing.length === 0,
    missing,
    stt: data?.stt,
    llm: data?.llm ?? undefined,
    inputMonitoring,
    accessibility,
    accessibilityHint: isTauri ? a11yHint : '',
    refetch: () => {
      refetch();
    },
    openInputMonitoringSettings: openInputMon,
    openAccessibilitySettings: openA11y,
    recheckInputMonitoring: recheckInputMon,
    recheckAccessibility: recheckA11y,
  };
}
