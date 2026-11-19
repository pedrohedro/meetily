import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { toast } from 'sonner';

export type DebriefActionState =
  | 'proposed'
  | 'preparing'
  | 'prepared'
  | 'executing'
  | 'executed'
  | 'executed_with_diff'
  | 'failed'
  | 'unknown';

export interface DebriefItem {
  text: string;
  ts?: string;
  quote?: string;
  owner?: string;
  owner_inferred?: boolean;
  deadline?: string;
}

export interface DebriefAction {
  title: string;
  destination: string;
  detail: string;
  state: DebriefActionState;
  prepared?: { tool: string; arguments: Record<string, unknown>; human_summary: string } | null;
  outcome?: { approved_input?: unknown; executed_input?: unknown[]; result?: string | null } | null;
  error?: string | null;
}

export interface Debrief {
  meeting_id: string;
  status: 'running' | 'completed' | 'failed';
  error?: string | null;
  markdown_path?: string | null;
  result?: {
    title?: string;
    summary?: string;
    decisions?: DebriefItem[];
    tasks?: DebriefItem[];
    pending?: DebriefItem[];
    questions?: DebriefItem[];
  } | null;
  actions: DebriefAction[];
  updated_at: string;
}

/** Debrief state for one meeting, kept fresh by the backend's `debrief-updated` event. */
export function useDebrief(meetingId: string) {
  const [debrief, setDebrief] = useState<Debrief | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Responses can arrive out of order (or for a meeting we already left): only the
  // latest request for the current meeting may update state.
  const requestSeq = useRef(0);

  const refresh = useCallback(async () => {
    const seq = ++requestSeq.current;
    try {
      const value = await invoke<Debrief | null>('api_get_debrief', { meetingId });
      if (seq !== requestSeq.current || (value && value.meeting_id !== meetingId)) return;
      setDebrief(value);
      setError(null);
    } catch (e) {
      if (seq === requestSeq.current) setError(String(e));
    } finally {
      if (seq === requestSeq.current) setLoading(false);
    }
  }, [meetingId]);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;
    setDebrief(null);
    setError(null);
    setLoading(true);
    // Subscribe first, then load, so an update emitted in between is not missed.
    listen<{ meeting_id: string }>('debrief-updated', (event) => {
      if (event.payload.meeting_id === meetingId) refresh();
    }).then((fn) => {
      if (disposed) {
        fn();
        return;
      }
      unlisten = fn;
      refresh();
    });
    return () => {
      disposed = true;
      requestSeq.current++;
      unlisten?.();
    };
  }, [meetingId, refresh]);

  const call = useCallback(
    async (command: string, args: Record<string, unknown>) => {
      try {
        await invoke(command, { meetingId, ...args });
        return true;
      } catch (e) {
        toast.error('Debrief', { description: String(e) });
        return false;
      }
    },
    [meetingId],
  );

  return {
    debrief,
    loading,
    error,
    generate: () => call('api_run_debrief', {}),
    prepare: (index: number) => call('api_prepare_debrief_action', { index }),
    /** `reviewed` is the exact payload shown to the user; the backend rejects it if the draft changed. */
    execute: (index: number, reviewed: { tool: string; arguments: Record<string, unknown> }) =>
      call('api_execute_debrief_action', { index, reviewed }),
  };
}
