// Workflow approvals: the settings of a status that waits for a decision, the request on an
// issue (who is asked, what they said, approve / reject / hand over), the requests waiting for
// me on the start page, and the page behind the link in an approver's e-mail.
import { t } from '../i18n'
import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Check, CircleCheck, CircleX, Clock, Plus, Stamp, Trash, X } from 'lucide-react'
import { api, ago, dateTime, type ApprovalConfig, type ApprovalStepConfig, type Approver, type Issue, type IssueDetail, type Named, type Project, type Status } from '../api'
import { Avatar, Loading, Logo, Modal, Spinner, usePeople, useToast } from '../ui'
import { AddPersonButton, PersonPicker } from './fields'
import './Approvals.css'

export interface Approval {
  id: string
  issueId: string
  state: 'pending' | 'approved' | 'rejected' | 'cancelled'
  statusName: string
  requestedBy: Named | null
  createdAt: string
  decidedAt: string | null
  parallel: boolean
  steps: {
    id: string
    name: string
    state: 'waiting' | 'pending' | 'approved' | 'rejected' | 'skipped' | 'cancelled'
    needed: number
    dueAt: string | null
    approvers: { id: string; user: Named | null; email: string | null; decision: 'approved' | 'rejected' | null; comment: string | null; decidedAt: string | null; for: Named | null }[]
  }[]
  canDecide: string[]
  canManage: boolean
  issue?: Issue
}

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e))

const stepState = (s: Approval['steps'][number]['state']): string =>
  ({ waiting: t('Not yet'), pending: t('Waiting'), approved: t('Approved'), rejected: t('Rejected'), skipped: t('Skipped'), cancelled: t('Cancelled') })[s]

// ---- settings: the approval of a status ----------------------------------------------------------

function approverLabel(a: Approver, project: Project, teams: Named[], people: Named[]): string {
  switch (a.kind) {
    case 'user':
      return people.find((p) => p.id === a.id)?.name ?? t('Someone')
    case 'team':
      return t('Team {name}', { name: teams.find((x) => x.id === a.id)?.name ?? '?' })
    case 'role':
      return a.role === 'admin' ? t('Project admins') : t('Project members')
    case 'field':
      return t('Field “{name}”', { name: project.fields.find((f) => f.id === a.id)?.name ?? '?' })
    case 'assignee':
      return t('The assignee')
    case 'reporter':
      return t('The reporter')
    case 'requester':
      return t('The requester (on the portal)')
  }
}

const sameApprover = (a: Approver, b: Approver) => a.kind === b.kind && a.id === b.id && a.role === b.role

/** Whether a status waits for a decision, as a short label for its button. */
export function approvalSummary(s: Status): string {
  const n = s.approval?.steps.length ?? 0
  return n === 0 ? t('Approval') : n === 1 ? t('Approval: on') : t('Approval: {count} steps', { count: n })
}

export function ApprovalDialog({ status, project, onSave, onClose }: { status: Status; project: Project; onSave: (c: ApprovalConfig | null) => void; onClose: () => void }) {
  const teams = useQuery({ queryKey: ['teams'], queryFn: () => api.get<Named[]>('/teams') }).data ?? []
  const people = usePeople().data ?? []
  const others = project.statuses.filter((s) => s.id !== status.id)
  const c = status.approval
  const [steps, setSteps] = useState<ApprovalStepConfig[]>(c?.steps ?? [{ name: '', approvers: [], quorum: 'any', rejection: 'any' }])
  const [parallel, setParallel] = useState(c?.parallel ?? false)
  const [approved, setApproved] = useState(c?.approvedStatusId ?? others.find((s) => s.category === 'done')?.id ?? '')
  const [rejected, setRejected] = useState(c?.rejectedStatusId ?? others.find((s) => s.category === 'todo')?.id ?? '')
  const [self, setSelf] = useState(c?.selfApproval ?? false)
  const [remind, setRemind] = useState(c?.remindHours ? String(c.remindHours) : '')
  const [timeoutHours, setTimeoutHours] = useState(c?.timeout ? String(c.timeout.hours) : '')
  const [timeoutAction, setTimeoutAction] = useState<NonNullable<ApprovalConfig['timeout']>['action']>(c?.timeout?.action ?? 'reassign')
  const [timeoutUser, setTimeoutUser] = useState<string | null>(c?.timeout?.userId ?? null)
  const [pauseSla, setPauseSla] = useState(c?.pauseSla ?? false)

  const setStep = (n: number, patch: Partial<ApprovalStepConfig>) => setSteps((all) => all.map((s, i) => (i === n ? { ...s, ...patch } : s)))
  const personFields = project.fields.filter((f) => f.kind === 'user')
  const choices: { group: string; items: Approver[] }[] = [
    { group: t('From the issue'), items: [{ kind: 'assignee' }, { kind: 'reporter' }, { kind: 'requester' }, ...personFields.map((f): Approver => ({ kind: 'field', id: f.id }))] },
    { group: t('Project roles'), items: [{ kind: 'role', role: 'admin' }, { kind: 'role', role: 'member' }] },
    { group: t('Teams'), items: teams.map((x): Approver => ({ kind: 'team', id: x.id })) },
    { group: t('People'), items: people.map((p): Approver => ({ kind: 'user', id: p.id })) },
  ]
  const hours = (v: string) => (v.trim() ? Number(v) : null)
  const problem =
    steps.some((s) => s.approvers.length === 0)
      ? t('Choose who approves in every step.')
      : !approved || !rejected
        ? t('Choose where issues go when they’re approved and when they’re rejected.')
        : steps.some((s) => (s.quorum === 'count' || s.quorum === 'percent') && !(Number(s.value) >= 1))
          ? t('Say how many must approve.')
          : timeoutHours.trim() && timeoutAction === 'reassign' && !timeoutUser
            ? t('Choose who a late step is handed to.')
            : ''
  const save = () => {
    const out: ApprovalConfig = {
      steps: steps.map((s) => ({
        name: s.name.trim(),
        approvers: s.approvers,
        quorum: s.quorum ?? 'any',
        ...(s.quorum === 'count' || s.quorum === 'percent' ? { value: Number(s.value) } : {}),
        rejection: s.rejection ?? 'any',
      })),
      parallel: steps.length > 1 && parallel,
      approvedStatusId: approved,
      rejectedStatusId: rejected,
      selfApproval: self,
      pauseSla,
    }
    const r = hours(remind)
    if (r) out.remindHours = r
    const th = hours(timeoutHours)
    if (th) out.timeout = { hours: th, action: timeoutAction, ...(timeoutAction === 'reassign' ? { userId: timeoutUser! } : {}) }
    onSave(out)
  }
  return (
    <Modal
      title={t('Approval in {name}', { name: status.name })}
      onClose={onClose}
      width={640}
      footer={
        <>
          {c && <button className="btn btn-danger" onClick={() => onSave(null)}>{t('Remove the approval')}</button>}
          <span className="spacer" />
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={!!problem} onClick={save}>{t('Save')}</button>
        </>
      }
    >
      <p className="muted small" style={{ marginTop: 0 }}>
        {t('An issue that enters this status waits for a decision. When it’s approved or rejected it moves on by itself; until then nobody takes those two moves by hand. Moving it anywhere else cancels the request.')}
      </p>
      {steps.map((s, n) => (
        <fieldset key={n} className="ps-fieldset ap-step">
          <legend>{steps.length > 1 ? t('Step {n}', { n: n + 1 }) : t('Who approves')}</legend>
          {steps.length > 1 && (
            <div className="row" style={{ gap: 8 }}>
              <input className="input input-sm" style={{ flex: 1 }} value={s.name} maxLength={60} placeholder={t('Name, e.g. Finance')} aria-label={t('Name of step {n}', { n: n + 1 })} onChange={(e) => setStep(n, { name: e.target.value })} />
              <button className="icon-btn" title={t('Remove this step')} aria-label={t('Remove step {n}', { n: n + 1 })} onClick={() => setSteps((all) => all.filter((_, i) => i !== n))}><Trash size={15} /></button>
            </div>
          )}
          <div className="ap-chips">
            {s.approvers.map((a, i) => (
              <span key={i} className="badge ap-chip">
                {approverLabel(a, project, teams, people)}
                <button className="ap-chip-x" aria-label={t('Remove {name}', { name: approverLabel(a, project, teams, people) })} onClick={() => setStep(n, { approvers: s.approvers.filter((_, j) => j !== i) })}><X size={12} /></button>
              </span>
            ))}
            <select
              className="select input-sm ps-auto"
              aria-label={t('Add an approver')}
              value=""
              onChange={(e) => {
                const [g, i] = e.target.value.split(':').map(Number)
                const a = choices[g]?.items[i]
                if (a && !s.approvers.some((x) => sameApprover(x, a))) setStep(n, { approvers: [...s.approvers, a] })
              }}
            >
              <option value="">{t('Add an approver…')}</option>
              {choices.map((g, gi) => g.items.length > 0 && (
                <optgroup key={gi} label={g.group}>
                  {g.items.map((a, i) => <option key={i} value={`${gi}:${i}`} disabled={s.approvers.some((x) => sameApprover(x, a))}>{approverLabel(a, project, teams, people)}</option>)}
                </optgroup>
              ))}
            </select>
          </div>
          <div className="row ap-quorum">
            <label className="small" htmlFor={`ap-q-${n}`}>{t('It takes')}</label>
            <select id={`ap-q-${n}`} className="select input-sm ps-auto" value={s.quorum ?? 'any'} onChange={(e) => setStep(n, { quorum: e.target.value as ApprovalStepConfig['quorum'], value: e.target.value === 'percent' ? 50 : e.target.value === 'count' ? 2 : undefined })}>
              <option value="any">{t('one approval')}</option>
              <option value="all">{t('everyone’s approval')}</option>
              <option value="count">{t('this many approvals:')}</option>
              <option value="percent">{t('this share of approvals (%):')}</option>
            </select>
            {(s.quorum === 'count' || s.quorum === 'percent') && (
              <input className="input input-sm ap-num" type="number" min={1} max={s.quorum === 'percent' ? 100 : 50} value={s.value ?? ''} aria-label={s.quorum === 'percent' ? t('Share in percent') : t('Number of approvals')} onChange={(e) => setStep(n, { value: e.target.value ? Number(e.target.value) : undefined })} />
            )}
          </div>
          {(s.quorum ?? 'any') !== 'any' && (
            <div className="row ap-quorum">
              <label className="small" htmlFor={`ap-r-${n}`}>{t('It’s rejected')}</label>
              <select id={`ap-r-${n}`} className="select input-sm ps-auto" value={s.rejection ?? 'any'} onChange={(e) => setStep(n, { rejection: e.target.value as ApprovalStepConfig['rejection'] })}>
                <option value="any">{t('as soon as one person rejects')}</option>
                <option value="quorum">{t('when the approvals can’t be reached any more')}</option>
              </select>
            </div>
          )}
        </fieldset>
      ))}
      <div className="row" style={{ gap: 12, marginBottom: 12, flexWrap: 'wrap' }}>
        {steps.length < 5 && <button className="btn btn-sm" onClick={() => setSteps((all) => [...all, { name: '', approvers: [], quorum: 'any', rejection: 'any' }])}><Plus size={14} /> {t('Add a step')}</button>}
        {steps.length > 1 && (
          <div className="seg" role="radiogroup" aria-label={t('Order of the steps')}>
            <button type="button" role="radio" aria-checked={!parallel} className={!parallel ? 'active' : ''} onClick={() => setParallel(false)}>{t('One after another')}</button>
            <button type="button" role="radio" aria-checked={parallel} className={parallel ? 'active' : ''} onClick={() => setParallel(true)}>{t('All at once')}</button>
          </div>
        )}
      </div>
      <div className="ap-grid">
        <div className="field">
          <label htmlFor="ap-yes">{t('When approved, move to')}</label>
          <select id="ap-yes" className="select" value={approved} onChange={(e) => setApproved(e.target.value)}>
            <option value="" disabled>{t('Choose a status')}</option>
            {others.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
          </select>
        </div>
        <div className="field">
          <label htmlFor="ap-no">{t('When rejected, move to')}</label>
          <select id="ap-no" className="select" value={rejected} onChange={(e) => setRejected(e.target.value)}>
            <option value="" disabled>{t('Choose a status')}</option>
            {others.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
          </select>
        </div>
        <div className="field">
          <label htmlFor="ap-remind">{t('Remind approvers every (hours)')}</label>
          <input id="ap-remind" className="input" type="number" min={1} max={720} value={remind} placeholder={t('never')} onChange={(e) => setRemind(e.target.value)} />
        </div>
        <div className="field">
          <label htmlFor="ap-timeout">{t('A step is late after (hours)')}</label>
          <input id="ap-timeout" className="input" type="number" min={1} max={8760} value={timeoutHours} placeholder={t('never')} onChange={(e) => setTimeoutHours(e.target.value)} />
        </div>
      </div>
      {timeoutHours.trim() && (
        <div className="row ap-quorum" style={{ marginBottom: 12 }}>
          <label className="small" htmlFor="ap-late">{t('A late step')}</label>
          <select id="ap-late" className="select input-sm ps-auto" value={timeoutAction} onChange={(e) => setTimeoutAction(e.target.value as typeof timeoutAction)}>
            <option value="reassign">{t('is also handed to')}</option>
            <option value="skip">{t('is skipped')}</option>
            <option value="reject">{t('is rejected')}</option>
          </select>
          {timeoutAction === 'reassign' && (
            <PersonPicker className="btn ps-person-btn" value={people.find((p) => p.id === timeoutUser) ?? null} onChange={setTimeoutUser} allowNone={false} label={t('Who a late step is handed to')} none={t('Choose a person')} />
          )}
        </div>
      )}
      <div className="ap-options">
        <label className="row small"><input type="checkbox" checked={self} onChange={(e) => setSelf(e.target.checked)} /> {t('Whoever moves the issue here may approve it too')}</label>
        <label className="row small"><input type="checkbox" checked={pauseSla} onChange={(e) => setPauseSla(e.target.checked)} /> {t('Service-level clocks stand still while an issue waits')}</label>
      </div>
      {problem && <p className="faint small" role="status" style={{ marginBottom: 0 }}>{problem}</p>}
    </Modal>
  )
}

// ---- on an issue -----------------------------------------------------------------------------------

function Decision({ d }: { d: 'approved' | 'rejected' | null }) {
  if (d === 'approved') return <CircleCheck size={15} className="ap-yes" aria-label={t('Approved')} />
  if (d === 'rejected') return <CircleX size={15} className="ap-no" aria-label={t('Rejected')} />
  return <Clock size={15} className="faint" aria-label={t('Waiting')} />
}

function Steps({ a }: { a: Approval }) {
  return (
    <ol className="ap-steps">
      {a.steps.map((s) => (
        <li key={s.id} className={`ap-step-row ap-${s.state}`}>
          <div className="row" style={{ gap: 8, flexWrap: 'wrap' }}>
            <strong>{s.name}</strong>
            <span className={`badge ${s.state === 'approved' ? 'badge-success' : s.state === 'rejected' ? 'badge-danger' : s.state === 'pending' ? 'badge-warning' : ''}`}>{stepState(s.state)}</span>
            {s.state === 'pending' && s.approvers.length > 1 && <span className="faint small">{t('{n} of {count} must approve', { n: s.needed, count: s.approvers.length })}</span>}
            {s.dueAt && <span className="faint small" title={dateTime(s.dueAt)}>{t('late {when}', { when: ago(s.dueAt) })}</span>}
          </div>
          {s.approvers.length > 0 && (
            <ul className="ap-voters">
              {s.approvers.map((v) => (
                <li key={v.id}>
                  <Decision d={v.decision} />
                  {v.user ? <Avatar name={v.user.name} id={v.user.id} size={20} decorative /> : null}
                  <span>
                    {v.user?.name ?? v.email}
                    {v.for && <span className="faint"> ({t('for {name}', { name: v.for.name })})</span>}
                    {v.comment && <span className="faint">: “{v.comment}”</span>}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </li>
      ))}
    </ol>
  )
}

/** The request an issue waits on: who is asked, and the caller's decision when it's theirs. */
export function ApprovalPanel({ issue, onChanged }: { issue: IssueDetail; onChanged: () => void }) {
  const qc = useQueryClient()
  const toast = useToast()
  const pending = !!issue.approval
  const q = useQuery({ queryKey: ['issue', issue.key, 'approvals'], queryFn: () => api.get<Approval[]>(`/issues/${issue.key}/approvals`), enabled: pending })
  const [comment, setComment] = useState('')
  const [busy, setBusy] = useState(false)
  const a = q.data?.find((x) => x.state === 'pending')
  if (!pending || !a) return null
  const mine = a.canDecide.length > 0
  const asked = a.steps.flatMap((s) => (s.state === 'pending' ? s.approvers.flatMap((v) => (v.user ? [v.user.id] : [])) : []))
  const act = async (f: () => Promise<unknown>, done: string) => {
    setBusy(true)
    try {
      await f()
      toast(done)
      setComment('')
      qc.invalidateQueries({ queryKey: ['my-approvals'] })
      onChanged()
    } catch (e) {
      toast(errText(e))
    } finally {
      setBusy(false)
    }
  }
  const decide = (decision: 'approve' | 'reject') =>
    act(() => api.post(`/approvals/${a.id}/decide`, { decision, comment: comment.trim() || undefined, stepId: a.canDecide[0] }), decision === 'approve' ? t('Approved') : t('Rejected'))
  const handOver = (p: Named) => act(() => api.post(`/approvals/${a.id}/delegate`, { userId: p.id, stepId: a.canDecide[0], comment: comment.trim() || undefined }), mine ? t('Handed to {name}', { name: p.name }) : t('{name} is asked too', { name: p.name }))
  return (
    <div className={`section ap-panel${mine ? ' ap-mine' : ''}`}>
      <div className="section-head">
        <h3><Stamp size={15} aria-hidden /> {t('Waiting for approval')}</h3>
        <span className="faint small">{a.requestedBy ? t('asked by {name} {when}', { name: a.requestedBy.name, when: ago(a.createdAt) }) : ago(a.createdAt)}</span>
      </div>
      <Steps a={a} />
      {mine && (
        <div className="ap-decide">
          <textarea className="textarea" rows={2} value={comment} maxLength={2000} placeholder={t('A comment for the history (optional)')} aria-label={t('Comment on your decision')} onChange={(e) => setComment(e.target.value)} />
          <div className="row" style={{ gap: 8, flexWrap: 'wrap' }}>
            <button className="btn btn-primary" disabled={busy} onClick={() => decide('approve')}><Check size={15} /> {t('Approve')}</button>
            <button className="btn btn-danger" disabled={busy} onClick={() => decide('reject')}><X size={15} /> {t('Reject')}</button>
            <span className="spacer" />
            <AddPersonButton exclude={asked} onAdd={handOver} label={t('Hand over to…')} />
          </div>
        </div>
      )}
      {!mine && a.canManage && <AddPersonButton exclude={asked} onAdd={handOver} label={t('Ask someone else too')} />}
    </div>
  )
}

// ---- the start page ----------------------------------------------------------------------------------

/** The requests waiting for the caller's decision (nothing when there are none). */
export function MyApprovals({ open }: { open: (key: string) => void }) {
  const q = useQuery({ queryKey: ['my-approvals'], queryFn: () => api.get<Approval[]>('/approvals'), refetchInterval: 300_000 })
  const list = (q.data ?? []).filter((a) => a.issue)
  if (list.length === 0) return null
  return (
    <section className="panel ap-home">
      <div className="panel-head"><h2><Stamp size={15} /> {t('Waiting for your approval')}</h2><span className="badge badge-warning">{list.length}</span></div>
      <div className="panel-body">
        {list.map((a) => {
          const step = a.steps.find((s) => a.canDecide.includes(s.id))
          return (
            <div key={a.id} className="issue-row" role="button" tabIndex={0} onClick={() => open(a.issue!.key)} onKeyDown={(e) => e.key === 'Enter' && open(a.issue!.key)}>
              <span className="issue-key" style={{ minWidth: 64 }}>{a.issue!.key}</span>
              <span className="ir-title">{a.issue!.title}</span>
              <span className="faint small" style={{ whiteSpace: 'nowrap' }}>{step?.name}{a.requestedBy ? ` · ${a.requestedBy.name}` : ''} · {ago(a.createdAt)}</span>
            </div>
          )
        })}
      </div>
    </section>
  )
}

// ---- the link in the e-mail (/approve/<token>) -----------------------------------------------------

interface LinkInfo {
  key: string
  title: string
  step: string
  requestedBy: string | null
  state: Approval['state']
  decision: 'approved' | 'rejected' | null
  open: boolean
  url: string
}

/** What the e-mail asks to decide. Opening the page decides nothing: the buttons do. */
export function ApprovePage() {
  const token = decodeURIComponent(location.pathname.replace(/^\/approve\//, '').replace(/\/$/, ''))
  const path = `/approval-links/${encodeURIComponent(token)}`
  const q = useQuery({ queryKey: ['approval-link', token], queryFn: () => api.get<LinkInfo>(path), retry: false })
  const [info, setInfo] = useState<LinkInfo | null>(null)
  const [comment, setComment] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const i = info ?? q.data
  const decide = async (decision: 'approve' | 'reject') => {
    setBusy(true)
    setErr('')
    try {
      setInfo(await api.post<LinkInfo>(path, { decision, comment: comment.trim() || undefined }))
    } catch (e) {
      setErr(errText(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <div className="auth">
      <main className="auth-card ap-link">
        <Logo />
        {q.isLoading && <Loading />}
        {q.error && (
          <>
            <h1>{t('This link doesn’t work')}</h1>
            <p className="muted">{t('It may be incomplete, or the request is gone. Sign in to see what waits for you.')}</p>
            <a className="btn" href="/">{t('Open TaskCraft')}</a>
          </>
        )}
        {i && (
          <>
            <h1>{i.open ? t('Your approval is needed') : i.decision === 'approved' ? t('You approved') : i.decision === 'rejected' ? t('You rejected') : t('Nothing left to decide')}</h1>
            <p className="ap-link-issue"><span className="issue-key">{i.key}</span> {i.title}</p>
            <p className="muted small">
              {t('Step: {step}', { step: i.step })}
              {i.requestedBy && <> · {t('asked by {name}', { name: i.requestedBy })}</>}
            </p>
            {i.open ? (
              <>
                <div className="field">
                  <label htmlFor="ap-link-comment">{t('Comment (optional)')}</label>
                  <textarea id="ap-link-comment" className="textarea" rows={3} maxLength={2000} value={comment} onChange={(e) => setComment(e.target.value)} />
                </div>
                {err && <p className="error-text" role="alert">{err}</p>}
                <div className="row" style={{ gap: 8 }}>
                  <button className="btn btn-primary" style={{ flex: 1 }} disabled={busy} onClick={() => decide('approve')}>{busy ? <Spinner /> : <Check size={15} />} {t('Approve')}</button>
                  <button className="btn btn-danger" style={{ flex: 1 }} disabled={busy} onClick={() => decide('reject')}><X size={15} /> {t('Reject')}</button>
                </div>
              </>
            ) : (
              <p className="muted">
                {i.decision ? t('Thank you: your decision is recorded.') : i.state === 'cancelled' ? t('The request was withdrawn.') : t('The request has been decided already.')}
              </p>
            )}
            <p className="small" style={{ marginBottom: 0 }}><a href={i.url}>{t('Open the issue in TaskCraft')}</a></p>
          </>
        )}
      </main>
    </div>
  )
}
