// Executing a test run: its cases on the left, the chosen one on the right with a result, a
// comment and files per step. A failure becomes a bug in one click.
import { t } from '../i18n'
import { useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Bug, Paperclip, Trash } from 'lucide-react'
import { api, ago, dateTime } from '../api'
import { ErrorBox, Loading, useToast } from '../ui'
import { RichView } from '../components/Editor'
import { RESULTS, ResultBadge, RunProgress, resultLabel, type RunLine, type TestResult } from './Tests'
import './Tests.css'

interface RunFile { id: string; step: number | null; name: string; mime: string; size: number; url: string }
interface RunCase {
  id: string
  case: { id: string; key: string; title: string; priority: string }
  version: number
  outdated: boolean
  kind: 'steps' | 'scenario'
  assignee: { id: string; name: string } | null
  result: TestResult
  comment: string | null
  steps: { action: string; expected: string; result: TestResult; comment: string | null }[]
  rendered: { preconditions: unknown; steps: { action: unknown; expected: unknown }[] }
  executedBy: string | null
  executedAt: string | null
  files: RunFile[]
  bugs: { id: string; key: string; title: string; status: { name: string; category: string } }[]
}
interface Run extends RunLine {
  projectKey: string
  testers: { id: string; name: string }[]
  canExecute: boolean
  canManage: boolean
  cases: RunCase[]
}

function ResultButtons({ value, onChange, disabled, label }: { value: TestResult; onChange: (r: TestResult) => void; disabled: boolean; label: string }) {
  return (
    <div className="seg tm-results" role="group" aria-label={label}>
      {RESULTS.map((r) => (
        <button key={r} type="button" disabled={disabled} className={`${value === r ? 'active' : ''} tm-r-${r}`} aria-pressed={value === r} onClick={() => onChange(value === r ? 'untested' : r)}>{resultLabel(r)}</button>
      ))}
    </div>
  )
}

export default function TestRun() {
  const { key, id } = useParams()
  const qc = useQueryClient()
  const toast = useToast()
  const nav = useNavigate()
  const q = useQuery({ queryKey: ['tests', key, 'run', id], queryFn: () => api.get<Run>(`/test-runs/${id}`) })
  const [chosen, setChosen] = useState<string | null>(null)
  if (q.isLoading) return <Loading />
  if (q.error || !q.data) return <div className="page"><ErrorBox error={q.error} /></div>
  const run = q.data
  const open = !run.finishedAt && run.canExecute
  const current = run.cases.find((c) => c.id === chosen) ?? run.cases.find((c) => c.result === 'untested') ?? run.cases[0]
  const refresh = () => qc.invalidateQueries({ queryKey: ['tests', key] })
  const act = async (f: () => Promise<unknown>) => {
    try { await f() } catch (e) { toast((e as Error).message) }
    await refresh()
  }
  return (
    <div className="page">
      <div className="crumbs"><Link to={`/projects/${key}/tests?tab=runs`}>{t('Test runs')}</Link></div>
      <div className="page-head">
        <h1><span className="issue-key">{run.key}</span> {run.name}</h1>
        {run.finishedAt ? <span className="badge">{t('Finished')}</span> : <span className="badge badge-accent">{t('Running')}</span>}
        <span className="spacer" />
        {run.canExecute && (run.finishedAt
          ? <button className="btn btn-sm" onClick={() => act(() => api.patch(`/test-runs/${run.id}`, { finished: false }))}>{t('Reopen')}</button>
          : <button className="btn btn-primary btn-sm" onClick={() => act(() => api.patch(`/test-runs/${run.id}`, { finished: true }))}>{t('Finish the run')}</button>)}
        {run.canManage && <button className="btn btn-sm btn-danger" onClick={() => { if (window.confirm(t('Delete this run with its results?'))) void act(async () => { await api.del(`/test-runs/${run.id}`); nav(`/projects/${key}/tests?tab=runs`) }) }}><Trash size={14} /> {t('Delete')}</button>}
      </div>
      <p className="small faint" style={{ marginTop: 0 }}>
        {[run.plan && t('Plan: {name}', { name: run.plan.name }), run.version && t('Release: {name}', { name: run.version.name }), run.environment && t('Environment: {name}', { name: run.environment }), run.testers.length > 0 && t('Testers: {names}', { names: run.testers.map((x) => x.name).join(', ') })].filter(Boolean).join(' · ')}
      </p>
      <div style={{ maxWidth: 520, marginBottom: 14 }}><RunProgress counts={run.counts} /></div>
      <div className="tm-run">
        <nav className="tm-run-list" aria-label={t('Test cases of the run')}>
          {run.cases.map((c) => (
            <button key={c.id} className={`tm-run-item ${current?.id === c.id ? 'active' : ''}`} aria-current={current?.id === c.id} onClick={() => setChosen(c.id)}>
              <span className="tm-run-title"><span className="issue-key">{c.case.key}</span> {c.case.title}</span>
              <ResultBadge result={c.result} />
            </button>
          ))}
        </nav>
        {current && <CasePane key={current.id} c={current} run={run} open={open} act={act} />}
      </div>
    </div>
  )
}

function CasePane({ c, run, open, act }: { c: RunCase; run: Run; open: boolean; act: (f: () => Promise<unknown>) => Promise<void> }) {
  const toast = useToast()
  const [comment, setComment] = useState(c.comment ?? '')
  const put = (body: Record<string, unknown>) => act(() => api.put(`/test-run-cases/${c.id}`, body))
  const upload = async (files: FileList | null, step?: number) => {
    if (!files?.length) return
    const form = new FormData()
    for (const f of files) form.append('file', f)
    await act(() => api.upload(`/test-run-cases/${c.id}/files${step ? `?step=${step}` : ''}`, form))
  }
  const fileList = (files: RunFile[]) => files.map((f) => (
    <span key={f.id} className="badge tm-file">
      <a href={f.url} target="_blank" rel="noreferrer">{f.name}</a>
      {open && <button className="ap-chip-x" aria-label={t('Remove {name}', { name: f.name })} onClick={() => act(() => api.del(`/test-files/${f.id}`))}>×</button>}
    </span>
  ))
  const attach = (label: string, step?: number) => open && (
    <label className="btn btn-ghost btn-sm tm-attach"><Paperclip size={13} /> {label}<input type="file" multiple hidden aria-label={label} onChange={(e) => { void upload(e.target.files, step); e.target.value = '' }} /></label>
  )
  return (
    <section className="panel tm-pane" aria-label={c.case.title}>
      <div className="panel-head">
        <h2><span className="issue-key">{c.case.key}</span> {c.case.title}</h2>
        <span className="spacer" />
        <span className="small faint">{t('Version {n}', { n: c.version })}{c.outdated && <> · {t('the test case has changed since')}</>}</span>
      </div>
      <div className="tm-pane-body">
        {!!c.rendered.preconditions && <><h3 className="tm-h">{t('Preconditions')}</h3><RichView doc={c.rendered.preconditions} label={t('Preconditions')} /></>}
        <ol className="tm-steps">
          {c.steps.map((s, i) => (
            <li key={i}>
              <div className="tm-step">
                <RichView doc={c.rendered.steps[i]?.action} label={t('Step {n}', { n: i + 1 })} />
                {!!s.expected && <div className="tm-expected"><span className="small faint">{t('Expected')}</span><RichView doc={c.rendered.steps[i]?.expected} label={t('Expected result of step {n}', { n: i + 1 })} /></div>}
                <div className="row-wrap tm-step-result">
                  <ResultButtons value={s.result} disabled={!open} label={t('Result of step {n}', { n: i + 1 })} onChange={(r) => put({ step: { index: i + 1, result: r } })} />
                  <input className="input input-sm" style={{ flex: 1, minWidth: 180 }} disabled={!open} aria-label={t('What happened in step {n}', { n: i + 1 })} placeholder={t('What happened instead?')} defaultValue={s.comment ?? ''}
                    onBlur={(e) => { if (e.target.value !== (s.comment ?? '')) void put({ step: { index: i + 1, comment: e.target.value } }) }} />
                  {attach(t('File for step {n}', { n: i + 1 }), i + 1)}
                </div>
                <div>{fileList(c.files.filter((f) => f.step === i + 1))}</div>
              </div>
            </li>
          ))}
        </ol>
        <h3 className="tm-h">{t('Result')}</h3>
        <div className="row-wrap" style={{ gap: 10 }}>
          <ResultButtons value={c.result} disabled={!open} label={t('Result of the test case')} onChange={(r) => put({ result: r })} />
          {c.executedAt && <span className="small faint" title={dateTime(c.executedAt)}>{c.executedBy} · {ago(c.executedAt)}</span>}
        </div>
        <div className="field" style={{ marginTop: 10 }}>
          <label htmlFor="tm-comment">{t('Comment')}</label>
          <textarea id="tm-comment" className="input" rows={2} disabled={!open} value={comment} onChange={(e) => setComment(e.target.value)} onBlur={() => { if (comment !== (c.comment ?? '')) void put({ comment }) }} />
        </div>
        <div className="row-wrap" style={{ gap: 6 }}>{fileList(c.files.filter((f) => f.step == null))}{attach(t('Attach a file'))}</div>
        {(c.result === 'failed' || c.result === 'blocked' || c.bugs.length > 0) && (
          <>
            <h3 className="tm-h">{t('Bugs')}</h3>
            <ul className="tm-plain">{c.bugs.map((b) => <li key={b.id}><Link to={`/issues/${b.key}`} className="issue-key">{b.key}</Link> {b.title} <span className="badge">{b.status.name}</span></li>)}</ul>
            {run.canExecute && (
              <button className="btn btn-sm" onClick={async () => {
                try {
                  const b = await api.post<{ key: string }>(`/test-run-cases/${c.id}/bug`, {})
                  toast(t('{key} created', { key: b.key }))
                } catch (e) { toast((e as Error).message) }
                await act(async () => {})
              }}><Bug size={14} /> {t('Create a bug')}</button>
            )}
          </>
        )}
      </div>
    </section>
  )
}
