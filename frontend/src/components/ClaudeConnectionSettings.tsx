'use client';

import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AlertTriangle, CheckCircle2, Copy, Loader2, RefreshCw, XCircle } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Switch } from '@/components/ui/switch';

interface ClaudeStatus {
  installed: boolean;
  path: string | null;
  searched: string[];
  version: string | null;
  logged_in: boolean | null;
  auth_method: string | null;
  plan: string | null;
  login_expires_at: string | null;
  error: string | null;
}

interface TestResult {
  ok: boolean;
  duration_ms: number;
  message: string;
}

const LOGIN_COMMAND = 'claude auth login';

function daysUntil(iso: string | null): number | null {
  if (!iso) return null;
  const ms = new Date(iso).getTime() - Date.now();
  return Number.isNaN(ms) ? null : Math.ceil(ms / 86_400_000);
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-start justify-between gap-4 py-3 border-b border-line-soft last:border-0">
      <div className="text-sm text-fg-muted shrink-0 w-32">{label}</div>
      <div className="text-sm text-fg-strong text-right min-w-0 break-words">{children}</div>
    </div>
  );
}

export function ClaudeConnectionSettings() {
  const [status, setStatus] = useState<ClaudeStatus | null>(null);
  const [checking, setChecking] = useState(true);
  const [testing, setTesting] = useState(false);
  const [test, setTest] = useState<TestResult | null>(null);
  const [testError, setTestError] = useState<string | null>(null);
  const [autoDebrief, setAutoDebrief] = useState<boolean | null>(null);

  const refresh = useCallback(async () => {
    setChecking(true);
    try {
      setStatus(await invoke<ClaudeStatus>('api_claude_status'));
    } catch (e) {
      setStatus({
        installed: false, path: null, searched: [], version: null, logged_in: null,
        auth_method: null, plan: null, login_expires_at: null, error: String(e),
      });
    } finally {
      setChecking(false);
    }
  }, []);

  useEffect(() => {
    refresh();
    invoke<{ auto_debrief: boolean }>('get_recording_preferences')
      .then((p) => setAutoDebrief(p.auto_debrief))
      .catch(() => setAutoDebrief(true));
  }, [refresh]);

  const runTest = async () => {
    setTesting(true);
    setTest(null);
    setTestError(null);
    try {
      setTest(await invoke<TestResult>('api_claude_test'));
    } catch (e) {
      setTestError(String(e));
    } finally {
      setTesting(false);
    }
  };

  const toggleAutoDebrief = async (enabled: boolean) => {
    const previous = autoDebrief;
    setAutoDebrief(enabled);
    try {
      // The whole preferences object is saved, so read the current one first.
      const prefs = await invoke<Record<string, unknown>>('get_recording_preferences');
      await invoke('set_recording_preferences', { preferences: { ...prefs, auto_debrief: enabled } });
    } catch (e) {
      setAutoDebrief(previous);
      toast.error('Could not save the preference', { description: String(e) });
    }
  };

  const copyLogin = async () => {
    await navigator.clipboard.writeText(LOGIN_COMMAND);
    toast.success('Command copied', { description: 'Paste it in Terminal, finish the sign-in in the browser, then press Check again.' });
  };

  const days = daysUntil(status?.login_expires_at ?? null);
  const signedIn = status?.logged_in === true;
  const ready = !!status?.installed && signedIn && (days === null || days > 0);

  return (
    <div className="space-y-6">
      <div className="bg-background rounded-lg border border-line p-6 shadow-sm">
        <div className="flex items-start justify-between gap-4 mb-2">
          <div>
            <h3 className="text-lg font-semibold text-fg-strong">Claude connection</h3>
            <p className="text-sm text-fg-muted mt-1">
              The Debrief tab uses the Claude Code app on this Mac, signed in with your own Claude account. Meetily never
              sees or stores your credentials. The model that writes meeting summaries is separate (Settings → Summary).
            </p>
          </div>
          <Button size="sm" variant="outline" onClick={refresh} disabled={checking}>
            {checking ? <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" /> : <RefreshCw className="mr-1 h-3.5 w-3.5" />}
            Check again
          </Button>
        </div>

        <div className="mt-4">
          <Row label="Status">
            {checking && !status ? (
              <span className="text-fg-muted">Checking…</span>
            ) : ready ? (
              <span className="inline-flex items-center gap-1 text-green-600 dark:text-green-400">
                <CheckCircle2 className="h-4 w-4" /> Ready
              </span>
            ) : (
              <span className="inline-flex items-center gap-1 text-red-600 dark:text-red-400">
                <XCircle className="h-4 w-4" /> {!status?.installed ? 'Claude Code not found' : !signedIn ? 'Not signed in' : 'Login expired'}
              </span>
            )}
          </Row>

          <Row label="Claude Code">
            {status?.installed ? (
              <>
                {status.version ?? 'installed'}
                <div className="text-xs text-fg-subtle font-mono mt-0.5">{status.path}</div>
              </>
            ) : (
              <>
                Not found
                <div className="text-xs text-fg-subtle mt-0.5">Looked in:</div>
                {status?.searched.map((p) => (
                  <div key={p} className="text-xs text-fg-subtle font-mono">{p}</div>
                ))}
              </>
            )}
          </Row>

          {status?.installed && (
            <>
              <Row label="Account">
                {signedIn ? (
                  <>
                    Signed in{status.auth_method ? ` with ${status.auth_method}` : ''}
                    {status.plan && <span className="text-fg-muted"> · {status.plan} plan</span>}
                  </>
                ) : (
                  <span className="text-fg-muted">Not signed in</span>
                )}
              </Row>
              {signedIn && status.login_expires_at && (
                <Row label="Login valid until">
                  {new Date(status.login_expires_at).toLocaleDateString('en-GB', { day: 'numeric', month: 'short', year: 'numeric' })}
                  {days !== null && days <= 7 && (
                    <div className="mt-0.5 inline-flex items-center gap-1 text-xs text-amber-600 dark:text-amber-400">
                      <AlertTriangle className="h-3.5 w-3.5" />
                      {days > 0 ? `expires in ${days} day${days === 1 ? '' : 's'}: sign in again soon` : 'expired: sign in again'}
                    </div>
                  )}
                </Row>
              )}
            </>
          )}
          {status?.error && status.installed && <Row label="Note"><span className="text-red-600 dark:text-red-400">{status.error}</span></Row>}
        </div>

        {(!status?.installed || !signedIn || (days !== null && days <= 0)) && !checking && (
          <div className="mt-4 rounded-md border border-line bg-surface-subtle p-4 space-y-2">
            <div className="text-sm text-fg-soft">
              {!status?.installed
                ? 'Install Claude Code, then sign in from Terminal with:'
                : 'Sign in from Terminal with:'}
            </div>
            <div className="flex items-center gap-2">
              <code className="flex-1 rounded bg-surface-muted px-3 py-2 text-sm font-mono text-fg-strong">{LOGIN_COMMAND}</code>
              <Button size="sm" variant="outline" onClick={copyLogin}>
                <Copy className="mr-1 h-3.5 w-3.5" /> Copy
              </Button>
            </div>
            <div className="text-xs text-fg-subtle">Finish the sign-in in the browser, then press “Check again”.</div>
          </div>
        )}

        <div className="mt-5 flex flex-wrap items-center gap-3">
          <Button onClick={runTest} disabled={testing || !status?.installed}>
            {testing && <Loader2 className="mr-1 h-4 w-4 animate-spin" />}
            Test connection
          </Button>
          {testing && <span className="text-xs text-fg-subtle">A real round trip: it can take up to a minute and uses a tiny bit of your Claude usage.</span>}
          {test && (
            <span className={`inline-flex items-center gap-1 text-sm ${test.ok ? 'text-green-600 dark:text-green-400' : 'text-red-600 dark:text-red-400'}`}>
              {test.ok ? <CheckCircle2 className="h-4 w-4" /> : <XCircle className="h-4 w-4" />}
              {test.message}
            </span>
          )}
          {testError && <span className="text-sm text-red-600 dark:text-red-400">{testError}</span>}
        </div>
      </div>

      <div className="bg-background rounded-lg border border-line p-6 shadow-sm">
        <div className="flex items-center justify-between gap-4">
          <div>
            <h3 className="text-lg font-semibold text-fg-strong mb-1">Debrief after each meeting</h3>
            <p className="text-sm text-fg-muted">
              When the summary is ready, Claude lists tasks, pending promises and decisions in the meeting’s Debrief tab and
              in ~/markdowns/meetings. It is read-only: you approve any follow-up action yourself.
            </p>
          </div>
          <Switch checked={autoDebrief ?? true} onCheckedChange={toggleAutoDebrief} disabled={autoDebrief === null} />
        </div>
      </div>
    </div>
  );
}
