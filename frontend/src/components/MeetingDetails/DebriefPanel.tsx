'use client';

import { useState } from 'react';
import { AlertTriangle, CheckCircle2, Copy, Loader2, RefreshCw, Sparkles } from 'lucide-react';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { DebriefAction, DebriefItem, useDebrief } from '@/hooks/meeting-details/useDebrief';

const DESTINATION_LABEL: Record<string, string> = {
  'izzyplan-card': 'Izzyplan card',
  email: 'Email draft',
  'calendar-event': 'Calendar event',
  'teams-message': 'Teams message',
  'notion-page': 'Notion page',
  none: 'FYI',
};

/** Executing these sends something to other people; it cannot be undone. */
const SENDS_TO_OTHERS = new Set(['calendar-event', 'teams-message']);

function Section({ title, items, task = false }: { title: string; items?: DebriefItem[]; task?: boolean }) {
  if (!items?.length) return null;
  return (
    <section className="space-y-2">
      <h3 className="text-sm font-semibold text-fg-strong">
        {title} <span className="font-normal text-fg-subtle">({items.length})</span>
      </h3>
      <ul className="space-y-2">
        {items.map((item, i) => (
          <li key={i} className="rounded-md border bg-background p-3 text-sm">
            <div className="text-fg-strong">{item.text}</div>
            {task && (
              <div className="mt-1 text-xs text-fg-muted">
                Owner: {item.owner || 'not said'}
                {item.owner_inferred ? ' (inferred)' : ''} · Deadline: {item.deadline || 'not said'}
              </div>
            )}
            {(item.ts || item.quote) && (
              <div className="mt-1 text-xs text-fg-subtle">
                {item.ts && <span className="mr-1 font-mono">[{item.ts}]</span>}
                {item.quote && <span className="italic">“{item.quote}”</span>}
              </div>
            )}
          </li>
        ))}
      </ul>
    </section>
  );
}

function ActionCard({
  action,
  busy,
  onPrepare,
  onReview,
}: {
  action: DebriefAction;
  busy: boolean;
  onPrepare: () => void;
  onReview: () => void;
}) {
  const executable = action.destination !== 'none';
  const working = action.state === 'preparing' || action.state === 'executing';
  return (
    <li className="rounded-md border bg-background p-3 text-sm space-y-2">
      <div className="flex items-start justify-between gap-2">
        <div>
          <div className="font-medium text-fg-strong">{action.title}</div>
          {action.detail && <div className="mt-0.5 text-xs text-fg-muted">{action.detail}</div>}
        </div>
        <span className="shrink-0 rounded-full bg-surface-muted px-2 py-0.5 text-xs text-fg-soft">
          {DESTINATION_LABEL[action.destination] ?? action.destination}
        </span>
      </div>

      {action.error && <div className="text-xs text-red-600 dark:text-red-400">{action.error}</div>}

      {action.state === 'executed' && (
        <div className="flex items-center gap-1 text-xs text-green-700 dark:text-green-300">
          <CheckCircle2 className="h-3.5 w-3.5" /> Done{action.outcome?.result ? `: ${action.outcome.result}` : ''}
        </div>
      )}
      {(action.state === 'executed_with_diff' || action.state === 'unknown') && (
        <details className="text-xs text-amber-700 dark:text-amber-300">
          <summary className="flex cursor-pointer items-center gap-1">
            <AlertTriangle className="h-3.5 w-3.5" />
            {action.state === 'unknown' ? 'Check the destination before redoing this' : 'Ran with different arguments than approved'}
          </summary>
          <pre className="mt-1 max-h-48 overflow-auto rounded bg-surface-subtle p-2 text-[11px] text-fg-soft">
            {JSON.stringify(action.outcome ?? {}, null, 2)}
          </pre>
        </details>
      )}

      {executable && (
        <div className="flex flex-wrap gap-2">
          {working && (
            <span className="flex items-center gap-1 text-xs text-fg-muted">
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
              {action.state === 'preparing' ? 'Preparing…' : 'Executing…'}
            </span>
          )}
          {(action.state === 'proposed' || action.state === 'failed') && (
            <Button size="sm" variant="outline" disabled={busy} onClick={onPrepare}>
              Prepare
            </Button>
          )}
          {action.state === 'prepared' && (
            <>
              <Button size="sm" disabled={busy} onClick={onReview}>
                Review &amp; confirm
              </Button>
              <Button size="sm" variant="ghost" disabled={busy} onClick={onPrepare}>
                Re-prepare
              </Button>
            </>
          )}
        </div>
      )}
    </li>
  );
}

interface Review {
  index: number;
  destination: string;
  tool: string;
  arguments: Record<string, unknown>;
  humanSummary: string;
}

export function DebriefPanel({ meetingId }: { meetingId: string }) {
  const { debrief, loading, error, generate, prepare, execute } = useDebrief(meetingId);
  // A snapshot taken when the dialog opens: refreshes can't change what is being confirmed.
  const [review, setReview] = useState<Review | null>(null);
  const [confirming, setConfirming] = useState(false);

  const actions = debrief?.actions ?? [];
  const running = debrief?.status === 'running';
  const busy = running || actions.some((a) => a.state === 'preparing' || a.state === 'executing');
  const hasHistory = actions.some((a) => ['executing', 'executed', 'executed_with_diff', 'unknown'].includes(a.state));
  const result = debrief?.result;

  const openReview = (index: number) => {
    const action = actions[index];
    if (!action?.prepared) return;
    setReview({
      index,
      destination: action.destination,
      tool: action.prepared.tool,
      arguments: action.prepared.arguments,
      humanSummary: action.prepared.human_summary,
    });
  };

  const confirm = async () => {
    if (!review || confirming) return;
    setConfirming(true);
    await execute(review.index, { tool: review.tool, arguments: review.arguments });
    setConfirming(false);
    setReview(null);
  };

  const copy = async () => {
    const lines = [
      result?.title ?? '',
      result?.summary ?? '',
      ...(result?.tasks ?? []).map((t) => `- [ ] ${t.text}${t.owner ? ` (${t.owner})` : ''}`),
      ...(result?.pending ?? []).map((p) => `- ${p.text}`),
    ];
    await navigator.clipboard.writeText(lines.filter(Boolean).join('\n'));
    toast.success('Debrief copied');
  };

  if (loading) {
    return <div className="p-6 text-sm text-fg-subtle">Loading debrief…</div>;
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-center justify-between gap-2 border-b px-4 py-2">
        <div className="text-sm font-medium text-fg-strong">Claude Debrief</div>
        <div className="flex gap-2">
          {result && (
            <Button size="sm" variant="outline" onClick={copy}>
              <Copy className="mr-1 h-3.5 w-3.5" /> Copy
            </Button>
          )}
          {debrief && (
            <Button
              size="sm"
              variant="outline"
              disabled={busy || hasHistory}
              title={hasHistory ? 'Actions were already executed; regenerating would erase their history' : undefined}
              onClick={generate}
            >
              <RefreshCw className="mr-1 h-3.5 w-3.5" /> Regenerate
            </Button>
          )}
        </div>
      </div>

      <div className="flex-1 min-h-0 overflow-auto space-y-5 p-4">
        {error && (
          <div className="rounded-md border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-950/40 p-3 text-sm text-red-700 dark:text-red-300">
            Could not read this debrief: {error}
          </div>
        )}
        {!debrief && !error && (
          <div className="flex flex-col items-center gap-3 py-16 text-center">
            <Sparkles className="h-8 w-8 text-fg-faint" />
            <div className="text-sm text-fg-muted">
              Claude reads this meeting and lists decisions, tasks, pending promises and follow-up actions.
            </div>
            <Button onClick={generate}>Generate debrief with Claude</Button>
          </div>
        )}

        {running && (
          <div className="flex items-center gap-2 py-16 justify-center text-sm text-fg-muted">
            <Loader2 className="h-4 w-4 animate-spin" /> Claude is reading the meeting… (usually 2–3 min)
          </div>
        )}

        {debrief?.status === 'failed' && (
          <div className="rounded-md border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-950/40 p-3 text-sm text-red-700 dark:text-red-300 space-y-2">
            <div>Debrief failed: {debrief.error}</div>
            <Button size="sm" variant="outline" onClick={generate} disabled={busy}>
              Try again
            </Button>
          </div>
        )}

        {debrief?.status === 'completed' && !result && (
          <div className="text-sm text-fg-muted">
            This debrief was made by an older version and has no structured data. Regenerate to see it here.
          </div>
        )}

        {debrief?.status === 'completed' && result && (
          <>
            {result.summary && <p className="text-sm leading-relaxed text-fg">{result.summary}</p>}
            <Section title="Decisions" items={result.decisions} />
            <Section title="Tasks" items={result.tasks} task />
            <Section title="Pending promises" items={result.pending} />
            <Section title="Open questions & risks" items={result.questions} />
            {actions.length > 0 && (
              <section className="space-y-2">
                <h3 className="text-sm font-semibold text-fg-strong">Proposed actions</h3>
                <ul className="space-y-2">
                  {actions.map((action, i) => (
                    <ActionCard
                      key={i}
                      action={action}
                      busy={busy}
                      onPrepare={() => prepare(i)}
                      onReview={() => openReview(i)}
                    />
                  ))}
                </ul>
              </section>
            )}
            {debrief.markdown_path && (
              <div className="text-xs text-fg-faint break-all">Saved to {debrief.markdown_path}</div>
            )}
          </>
        )}
      </div>

      <Dialog open={!!review} onOpenChange={(open) => !open && !confirming && setReview(null)}>
        <DialogContent className="max-w-lg">
          <DialogHeader>
            <DialogTitle>Confirm action</DialogTitle>
            <DialogDescription>{review?.humanSummary}</DialogDescription>
          </DialogHeader>
          {review && SENDS_TO_OTHERS.has(review.destination) && (
            <div className="flex items-start gap-2 rounded-md border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-950/40 p-2 text-xs text-amber-800 dark:text-amber-200">
              <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" />
              This sends to other people and cannot be undone.
            </div>
          )}
          <div className="text-xs text-fg-subtle">Tool: {review?.tool}</div>
          <pre className="max-h-64 overflow-auto rounded bg-surface-subtle p-2 text-[11px] text-fg">
            {JSON.stringify(review?.arguments ?? {}, null, 2)}
          </pre>
          <DialogFooter>
            <Button variant="outline" disabled={confirming} onClick={() => setReview(null)}>
              Cancel
            </Button>
            <Button disabled={confirming} onClick={confirm}>
              {confirming && <Loader2 className="mr-1 h-3.5 w-3.5 animate-spin" />}
              Confirm and execute
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
