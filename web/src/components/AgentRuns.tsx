// The AI agents' work on an issue: queued or running (with a live log and a way to stop it),
// and how earlier runs ended — a draft pull request, an answer, or why it failed.
import { t } from '../i18n'
import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Bot, ChevronDown, ChevronRight, CircleStop } from 'lucide-react'
import { api, ago, dateTime, type IssueDetail, type Named } from '../api'
import { Spinner, useToast } from '../ui'
import './Approvals.css'

export interface AgentRun {
  id: string
  agent: { name: string; handle: string }
  state: 'queued' | 'running' | 'succeeded' | 'failed' | 'cancelled'
  trigger: 'assigned' | 'mentioned'
  triggeredBy: Named | null
  createdAt: string
  startedAt: string | null
  finishedAt: string | null
  result: { kind: 'answer' | 'pull_request'; summary?: string; branch?: string; url?: string; number?: number | null; files?: number } | null
  error: string | null
  modelCalls: number
  tokensIn: number
  tokensOut: number
  issue: { key: string; title: string }
  canCancel: boolean
}

export function runStateLabel(s: AgentRun['state']): string {
  return { queued: t('Queued'), running: t('Working'), succeeded: t('Done [[agent]]'), failed: t('Failed'), cancelled: t('Stopped') }[s]
}

export function runStateBadge(s: AgentRun['state']): string {
  return s === 'succeeded' ? 'badge-success' : s === 'failed' ? 'badge-danger' : s === 'running' || s === 'queued' ? 'badge-warning' : ''
}

function RunLog({ run }: { run: AgentRun }) {
  const active = run.state === 'queued' || run.state === 'running'
  const q = useQuery({
    queryKey: ['agent-run-log', run.id],
    queryFn: () => api.get<{ state: string; lines: { id: number; at: string; line: string }[] }>(`/agent-runs/${run.id}/log`),
    refetchInterval: active ? 3000 : false,
  })
  if (q.isLoading) return <Spinner />
  const lines = q.data?.lines ?? []
  if (lines.length === 0) return <div className="faint small">{t('Nothing logged yet.')}</div>
  return (
    <ol className="agent-log" aria-label={t('Log of the run')}>
      {lines.map((l) => (
        <li key={l.id}><time className="faint" dateTime={l.at} title={dateTime(l.at)}>{new Date(l.at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })}</time> {l.line}</li>
      ))}
    </ol>
  )
}

export default function AgentRuns({ issue }: { issue: IssueDetail }) {
  const qc = useQueryClient()
  const toast = useToast()
  const q = useQuery({ queryKey: ['issue', issue.key, 'agent-runs'], queryFn: () => api.get<AgentRun[]>(`/issues/${issue.key}/agent-runs`) })
  const [open, setOpen] = useState<string | null>(null)
  const runs = q.data ?? []
  if (runs.length === 0) return null
  const cancel = async (r: AgentRun) => {
    try {
      await api.post(`/agent-runs/${r.id}/cancel`)
    } catch (e) {
      toast((e as Error).message)
    }
    qc.invalidateQueries({ queryKey: ['issue', issue.key] })
  }
  return (
    <div className="section agent-runs">
      <div className="section-head">
        <h3><Bot size={15} aria-hidden /> {t('AI agents')}</h3>
      </div>
      <ul className="agent-run-list">
        {runs.map((r) => {
          const active = r.state === 'queued' || r.state === 'running'
          const shown = open === r.id || (open === null && active && r === runs[0])
          return (
            <li key={r.id} className="agent-run">
              <div className="row" style={{ gap: 8, flexWrap: 'wrap' }}>
                <button className="icon-btn icon-btn-sm" aria-expanded={shown} aria-label={shown ? t('Hide the log') : t('Show the log')} onClick={() => setOpen(shown ? '' : r.id)}>
                  {shown ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                </button>
                <strong>{r.agent.name}</strong>
                <span className={`badge ${runStateBadge(r.state)}`}>{r.state === 'running' && <Spinner size={11} />} {runStateLabel(r.state)}</span>
                <span className="faint small" title={dateTime(r.createdAt)}>
                  {r.trigger === 'assigned' ? t('assigned by {name}', { name: r.triggeredBy?.name ?? '?' }) : t('asked by {name}', { name: r.triggeredBy?.name ?? '?' })} · {ago(r.createdAt)}
                </span>
                <span className="spacer" />
                {r.canCancel && <button className="btn btn-sm" onClick={() => cancel(r)}><CircleStop size={13} /> {t('Stop')}</button>}
              </div>
              {r.state === 'queued' && <div className="faint small agent-run-note">{t('Waiting for a runner to pick it up.')}</div>}
              {r.result?.kind === 'pull_request' && r.result.url && (
                <div className="small agent-run-note">
                  <a href={r.result.url} target="_blank" rel="noreferrer noopener">{t('Draft pull request #{number}', { number: r.result.number ?? '' })}</a>
                  {r.result.files != null && <span className="faint"> · {t('{count} files changed', { count: r.result.files })}</span>}
                </div>
              )}
              {r.result?.kind === 'answer' && <div className="faint small agent-run-note">{t('Answered in a comment.')}</div>}
              {r.error && <div className="error-text small agent-run-note">{r.error}</div>}
              {shown && <RunLog run={r} />}
            </li>
          )
        })}
      </ul>
    </div>
  )
}
