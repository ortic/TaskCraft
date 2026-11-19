// Test management of a project: test cases in folders, test plans and run templates, test runs,
// and the reports (coverage, pass rates, flaky cases). /projects/<key>/tests?tab=…; a run is
// executed on /projects/<key>/tests/runs/<id> (TestRun.tsx).
import { t } from '../i18n'
import { useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Download, FlaskConical, FolderPlus, Play, Plus, Trash, Upload } from 'lucide-react'
import { api, ago, dateTime, qs, shortDate, type Project } from '../api'
import { Empty, ErrorBox, Loading, Modal, Tabs, usePeople, useProject, useToast } from '../ui'
import { RichView } from '../components/Editor'
import './Tests.css'

export type TestResult = 'untested' | 'passed' | 'failed' | 'blocked' | 'skipped'
export const RESULTS: TestResult[] = ['passed', 'failed', 'blocked', 'skipped']

export interface TestCaseLine {
  id: string
  key: string
  title: string
  folderId: string | null
  priority: 'low' | 'medium' | 'high' | 'critical'
  labels: string[]
  version: number
  kind: 'steps' | 'scenario'
  archived: boolean
  issueCount: number
  lastResult: TestResult | null
  lastRunAt: string | null
}

interface Folder { id: string; parentId: string | null; name: string }
interface CaseList { canManage: boolean; canExecute: boolean; folders: Folder[]; cases: TestCaseLine[] }
interface Step { action: string; expected: string }

interface CaseDetail extends TestCaseLine {
  projectKey: string
  preconditions: string | null
  steps: Step[]
  scenario: string | null
  rendered: { preconditions: unknown; steps: { action: unknown; expected: unknown }[] }
  canManage: boolean
  issues: { id: string; key: string; title: string; status: { name: string; category: string } }[]
  versions: { version: number; at: string; by: string | null }[]
  results: { run: { id: string; number: number; name: string }; result: TestResult; version: number; at: string | null; by: string | null }[]
}

export interface RunCounts { total: number; passed: number; failed: number; blocked: number; skipped: number; untested: number }
export interface RunLine {
  id: string
  key: string
  name: string
  plan: { id: string; name: string } | null
  version: { id: string; name: string } | null
  environment: string | null
  createdAt: string
  finishedAt: string | null
  counts: RunCounts
  passRate?: number | null
}

interface Plan { id: string; name: string; description: string | null; version: { id: string; name: string } | null; filter: { folderId?: string; labels?: string[]; priority?: string; text?: string } | null; caseCount: number; caseIds?: string[] }
interface Template { id: string; name: string; plan: { id: string; name: string }; environment: string | null; testerIds: string[] }

export interface TestReport {
  runs: RunLine[]
  history: { date: string; passed: number; failed: number; blocked: number; skipped: number }[]
  flaky: { case: { id: string; key: string; title: string }; flips: number; results: TestResult[] }[]
  failures: { case: { id: string; key: string; title: string } | null; result: TestResult; run: { id: string; key: string; name: string }; bugs: { key: string; title: string; status: { name: string; category: string } }[] }[]
  coverage: { issues: number; covered: number; passing: number; withoutTestsCount: number; failingCount: number; withoutTests: { key: string; title: string }[]; failing: { key: string; title: string; failing: number }[] }
  ready: boolean
}

export function resultLabel(r: TestResult | null | undefined): string {
  switch (r) {
    case 'passed': return t('Passed')
    case 'failed': return t('Failed')
    case 'blocked': return t('Blocked')
    case 'skipped': return t('Skipped')
    default: return t('Not run')
  }
}

export function ResultBadge({ result }: { result: TestResult | null | undefined }) {
  // the colour is a dot beside the word: the text keeps its contrast on every background
  return <span className="badge tm-res"><span aria-hidden className={`tm-dot tm-${result ?? 'untested'}`} />{resultLabel(result)}</span>
}

export function priorityLabel(p: string): string {
  return { low: t('Low'), medium: t('Medium'), high: t('High'), critical: t('Critical') }[p] ?? p
}

/** How far a run is: one bar, its parts labelled below. */
export function RunProgress({ counts }: { counts: RunCounts }) {
  const pct = (n: number) => `${counts.total ? (n / counts.total) * 100 : 0}%`
  const label = [`${counts.passed} ${t('passed [[count]]')}`, counts.failed ? `${counts.failed} ${t('failed [[count]]')}` : '', counts.blocked ? `${counts.blocked} ${t('blocked [[count]]')}` : '', counts.skipped ? `${counts.skipped} ${t('skipped [[count]]')}` : '', counts.untested ? `${counts.untested} ${t('to do [[count]]')}` : ''].filter(Boolean).join(' · ')
  return (
    <div className="tm-progress">
      <div className="tm-bar" role="img" aria-label={label}>
        <span className="tm-passed" style={{ width: pct(counts.passed) }} />
        <span className="tm-failed" style={{ width: pct(counts.failed) }} />
        <span className="tm-blocked" style={{ width: pct(counts.blocked) }} />
        <span className="tm-skipped" style={{ width: pct(counts.skipped) }} />
      </div>
      <span className="small faint">{label}</span>
    </div>
  )
}

type Tab = 'cases' | 'plans' | 'runs' | 'report'

export default function Tests() {
  const { key } = useParams()
  const project = useProject(key)
  const [params, setParams] = useSearchParams()
  const tab = (params.get('tab') as Tab) || 'cases'
  if (project.isLoading) return <Loading />
  if (project.error || !project.data) return <div className="page"><ErrorBox error={project.error} /></div>
  const p = project.data
  return (
    <div className="page">
      <div className="page-head"><h1><FlaskConical size={20} /> {t('Tests')}</h1></div>
      <Tabs
        value={tab}
        onChange={(id) => setParams(id === 'cases' ? {} : { tab: id })}
        tabs={[{ id: 'cases', label: t('Test cases') }, { id: 'plans', label: t('Test plans') }, { id: 'runs', label: t('Test runs') }, { id: 'report', label: t('Coverage and reports') }]}
      />
      <div style={{ marginTop: 14 }}>
        {tab === 'cases' && <CasesTab project={p} />}
        {tab === 'plans' && <PlansTab project={p} />}
        {tab === 'runs' && <RunsTab project={p} />}
        {tab === 'report' && <ReportTab project={p} />}
      </div>
    </div>
  )
}

function folderOptions(folders: Folder[]): { id: string; label: string; depth: number }[] {
  const out: { id: string; label: string; depth: number }[] = []
  const walk = (parent: string | null, depth: number, prefix: string) => {
    for (const f of folders.filter((x) => x.parentId === parent)) {
      out.push({ id: f.id, label: prefix + f.name, depth })
      if (depth < 10) walk(f.id, depth + 1, `${prefix}${f.name} / `)
    }
  }
  walk(null, 0, '')
  return out
}

// ---- test cases ---------------------------------------------------------------------------------------

function CasesTab({ project }: { project: Project }) {
  const qc = useQueryClient()
  const toast = useToast()
  const [params, setParams] = useSearchParams()
  const [folder, setFolder] = useState('')
  const [q, setQ] = useState('')
  const [priority, setPriority] = useState('')
  const [archived, setArchived] = useState(false)
  const [importing, setImporting] = useState(false)
  const list = useQuery({ queryKey: ['tests', project.key, 'cases', folder, q, priority, archived], queryFn: () => api.get<CaseList>(`/projects/${project.key}/tests${qs({ folder: folder || null, q: q || null, priority: priority || null, archived: archived || null })}`) })
  const open = params.get('case')
  const setOpen = (k: string | null) => { const n = new URLSearchParams(params); k ? n.set('case', k) : n.delete('case'); setParams(n) }
  const refresh = () => qc.invalidateQueries({ queryKey: ['tests', project.key] })
  const d = list.data
  const folders = folderOptions(d?.folders ?? [])
  const addFolder = async () => {
    const name = window.prompt(folder ? t('Name of the folder below the chosen one') : t('Name of the folder'))
    if (!name?.trim()) return
    try { await api.post(`/projects/${project.key}/test-folders`, { name: name.trim(), parentId: folder || null }) } catch (e) { toast((e as Error).message) }
    refresh()
  }
  return (
    <div className="tm-cases">
      <nav className="tm-folders" aria-label={t('Folders')}>
        <button className={`tm-folder ${folder === '' ? 'active' : ''}`} onClick={() => setFolder('')}>{t('All test cases')}</button>
        {folders.map((f) => (
          <button key={f.id} className={`tm-folder ${folder === f.id ? 'active' : ''}`} style={{ paddingInlineStart: 10 + f.depth * 14 }} onClick={() => setFolder(f.id)}>{(d?.folders ?? []).find((x) => x.id === f.id)?.name}</button>
        ))}
        {d?.canManage && (
          <div className="row" style={{ gap: 6, marginTop: 8 }}>
            <button className="btn btn-sm" onClick={addFolder}><FolderPlus size={14} /> {t('Folder')}</button>
            {folder && <button className="icon-btn icon-btn-sm" aria-label={t('Delete the folder')} title={t('Delete the folder (its test cases stay)')} onClick={async () => { await api.del(`/test-folders/${folder}`); setFolder(''); refresh() }}><Trash size={13} /></button>}
          </div>
        )}
      </nav>
      <div style={{ minWidth: 0, flex: 1 }}>
        <div className="row-wrap" style={{ gap: 8, marginBottom: 10 }}>
          <input className="input input-sm" style={{ width: 220 }} type="search" placeholder={t('Search test cases…')} aria-label={t('Search test cases')} value={q} onChange={(e) => setQ(e.target.value)} />
          <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Priority')} value={priority} onChange={(e) => setPriority(e.target.value)}>
            <option value="">{t('Any priority')}</option>
            {['critical', 'high', 'medium', 'low'].map((x) => <option key={x} value={x}>{priorityLabel(x)}</option>)}
          </select>
          <label className="row small tm-check"><input type="checkbox" checked={archived} onChange={(e) => setArchived(e.target.checked)} /> {t('Archived too')}</label>
          <span className="spacer" />
          <a className="btn btn-sm" href={`/api/projects/${project.key}/tests/export.csv`}><Download size={14} /> {t('Export CSV')}</a>
          {d?.canManage && <button className="btn btn-sm" onClick={() => setImporting(true)}><Upload size={14} /> {t('Import CSV')}</button>}
          {d?.canManage && <button className="btn btn-primary btn-sm" onClick={() => setOpen('new')}><Plus size={14} /> {t('New test case')}</button>}
        </div>
        {list.isLoading && <Loading />}
        {list.error && <ErrorBox error={list.error} />}
        {d && d.cases.length === 0 && <Empty icon={<FlaskConical size={32} />} title={t('No test cases here yet')}>{t('A test case says how to check something: steps with what you expect, or a Given / When / Then scenario.')}</Empty>}
        {d && d.cases.length > 0 && (
          <table className="list tm-table">
            <thead><tr><th>{t('Key')}</th><th>{t('Title')}</th><th>{t('Priority')}</th><th>{t('Labels')}</th><th>{t('Covers')}</th><th>{t('Last result')}</th></tr></thead>
            <tbody>
              {d.cases.map((c) => (
                <tr key={c.id} className={c.archived ? 'tm-archived' : ''}>
                  <td><button className="link-btn issue-key" onClick={() => setOpen(c.key)}>{c.key}</button></td>
                  <td><button className="link-btn tm-title" onClick={() => setOpen(c.key)}>{c.title}</button></td>
                  <td className="small">{priorityLabel(c.priority)}</td>
                  <td className="small faint">{c.labels.join(', ')}</td>
                  <td className="small">{c.issueCount > 0 ? t('{count} issues', { count: c.issueCount }) : <span className="faint">—</span>}</td>
                  <td><ResultBadge result={c.lastResult} /> {c.lastRunAt && <span className="faint small">{ago(c.lastRunAt)}</span>}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
      {open && d && <CaseDialog project={project} caseKey={open === 'new' ? null : open} folders={folders} folder={folder} canManage={d.canManage} onClose={() => { setOpen(null); refresh() }} />}
      {importing && <ImportDialog project={project} onClose={() => { setImporting(false); refresh() }} />}
    </div>
  )
}

function CaseDialog({ project, caseKey, folders, folder, canManage, onClose }: { project: Project; caseKey: string | null; folders: { id: string; label: string }[]; folder: string; canManage: boolean; onClose: () => void }) {
  const qc = useQueryClient()
  const q = useQuery({ queryKey: ['tests', project.key, 'case', caseKey], queryFn: () => api.get<CaseDetail>(`/tests/${caseKey}`), enabled: !!caseKey })
  const [editing, setEditing] = useState(!caseKey)
  if (caseKey && q.isLoading) return <Modal title={caseKey} onClose={onClose} width={760}><Loading /></Modal>
  if (caseKey && (q.error || !q.data)) return <Modal title={caseKey} onClose={onClose} width={520}><ErrorBox error={q.error} /></Modal>
  const c = q.data
  if (editing) return <CaseForm project={project} existing={c} folders={folders} folder={folder} onClose={onClose} onSaved={() => { if (c) { setEditing(false); qc.invalidateQueries({ queryKey: ['tests', project.key] }) } else onClose() }} />
  if (!c) return null
  return (
    <Modal
      title={<><span className="issue-key">{c.key}</span> {c.title}</>}
      onClose={onClose}
      width={760}
      footer={<>
        {canManage && <button className="btn btn-danger" onClick={async () => { if (window.confirm(t('Delete {key} with its results? Archive it to keep them.', { key: c.key }))) { await api.del(`/tests/${c.id}`); onClose() } }}>{t('Delete')}</button>}
        {canManage && <button className="btn" onClick={async () => { await api.patch(`/tests/${c.id}`, { archived: !c.archived }); qc.invalidateQueries({ queryKey: ['tests', project.key] }) }}>{c.archived ? t('Restore') : t('Archive')}</button>}
        <span className="spacer" />
        {canManage && <button className="btn" onClick={() => setEditing(true)}>{t('Edit')}</button>}
        <button className="btn btn-primary" onClick={onClose}>{t('Close')}</button>
      </>}
    >
      <p className="small faint" style={{ marginTop: 0 }}>
        {t('Version {n}', { n: c.version })} · {priorityLabel(c.priority)}{c.labels.length > 0 && <> · {c.labels.join(', ')}</>}{c.archived && <> · {t('Archived')}</>}
      </p>
      {c.preconditions && <><h3 className="tm-h">{t('Preconditions')}</h3><RichView doc={c.rendered.preconditions} label={t('Preconditions')} /></>}
      <h3 className="tm-h">{c.kind === 'scenario' ? t('Scenario') : t('Steps')}</h3>
      {c.kind === 'scenario' ? <pre className="tm-scenario">{c.scenario}</pre> : (
        <ol className="tm-steps">
          {c.rendered.steps.map((s, i) => (
            <li key={i}><div className="tm-step"><RichView doc={s.action} label={t('Step {n}', { n: i + 1 })} />{!!s.expected && <div className="tm-expected"><span className="small faint">{t('Expected')}</span><RichView doc={s.expected} label={t('Expected result of step {n}', { n: i + 1 })} /></div>}</div></li>
          ))}
          {c.rendered.steps.length === 0 && <li className="faint">{t('No steps yet')}</li>}
        </ol>
      )}
      <h3 className="tm-h">{t('Covers')}</h3>
      {c.issues.length === 0 ? <p className="faint small">{t('No issues yet: edit the test case to say which stories or requirements it checks.')}</p> : (
        <ul className="tm-plain">{c.issues.map((i) => <li key={i.id}><Link to={`/issues/${i.key}`} className="issue-key">{i.key}</Link> {i.title} <span className="badge">{i.status.name}</span></li>)}</ul>
      )}
      {c.results.length > 0 && <>
        <h3 className="tm-h">{t('Results')}</h3>
        <ul className="tm-plain">{c.results.map((r, i) => <li key={i}><ResultBadge result={r.result} /> <Link to={`/projects/${project.key}/tests/runs/${r.run.id}`}>{r.run.name}</Link> <span className="faint small">{t('Version {n}', { n: r.version })}{r.at && <> · {r.by} · <span title={dateTime(r.at)}>{ago(r.at)}</span></>}</span></li>)}</ul>
      </>}
      {c.versions.length > 1 && <p className="small faint">{t('Changed {count} times, last by {name} {when}', { count: c.versions.length - 1, name: c.versions[0].by ?? '—', when: ago(c.versions[0].at) })}</p>}
    </Modal>
  )
}

function CaseForm({ project, existing, folders, folder, onClose, onSaved }: { project: Project; existing?: CaseDetail; folders: { id: string; label: string }[]; folder: string; onClose: () => void; onSaved: () => void }) {
  const [title, setTitle] = useState(existing?.title ?? '')
  const [folderId, setFolderId] = useState(existing?.folderId ?? folder)
  const [priority, setPriority] = useState<string>(existing?.priority ?? 'medium')
  const [labels, setLabels] = useState((existing?.labels ?? []).join(', '))
  const [kind, setKind] = useState<'steps' | 'scenario'>(existing?.kind ?? 'steps')
  const [preconditions, setPreconditions] = useState(existing?.preconditions ?? '')
  const [steps, setSteps] = useState<Step[]>(existing?.steps?.length ? existing.steps : [{ action: '', expected: '' }])
  const [scenario, setScenario] = useState(existing?.scenario ?? 'Given \nWhen \nThen ')
  const [issues, setIssues] = useState((existing?.issues ?? []).map((i) => i.key).join(', '))
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)
  const patch = (i: number, p: Partial<Step>) => setSteps((all) => all.map((s, n) => (n === i ? { ...s, ...p } : s)))
  const move = (i: number, by: number) => setSteps((all) => { const n = [...all]; const [s] = n.splice(i, 1); n.splice(i + by, 0, s); return n })
  const save = async () => {
    setBusy(true)
    setErr('')
    const body = {
      title: title.trim(), folderId: folderId || null, priority, kind, preconditions: preconditions.trim() || null,
      labels: labels.split(',').map((l) => l.trim()).filter(Boolean),
      steps: steps.filter((s) => s.action.trim()), scenario: kind === 'scenario' ? scenario : existing?.scenario ?? null,
      issues: issues.split(/[,\s]+/).map((k) => k.trim()).filter(Boolean),
    }
    try {
      if (existing) await api.patch(`/tests/${existing.id}`, body)
      else await api.post(`/projects/${project.key}/tests`, body)
      onSaved()
    } catch (e) {
      setErr((e as Error).message)
      setBusy(false)
    }
  }
  return (
    <Modal title={existing ? t('Edit {name}', { name: existing.key }) : t('New test case')} onClose={onClose} width={760}
      footer={<><button className="btn" onClick={onClose}>{t('Cancel')}</button><button className="btn btn-primary" disabled={busy || !title.trim()} onClick={save}>{existing ? t('Save') : t('Create test case')}</button></>}>
      <div className="field"><label htmlFor="tm-title">{t('Title')}</label><input id="tm-title" className="input" autoFocus maxLength={300} value={title} onChange={(e) => setTitle(e.target.value)} /></div>
      <div className="ap-grid">
        <div className="field"><label htmlFor="tm-folder">{t('Folder')}</label>
          <select id="tm-folder" className="select" value={folderId ?? ''} onChange={(e) => setFolderId(e.target.value)}><option value="">{t('No folder')}</option>{folders.map((f) => <option key={f.id} value={f.id}>{f.label}</option>)}</select></div>
        <div className="field"><label htmlFor="tm-priority">{t('Priority')}</label>
          <select id="tm-priority" className="select" value={priority} onChange={(e) => setPriority(e.target.value)}>{['critical', 'high', 'medium', 'low'].map((x) => <option key={x} value={x}>{priorityLabel(x)}</option>)}</select></div>
        <div className="field"><label htmlFor="tm-labels">{t('Labels')}</label><input id="tm-labels" className="input" placeholder={t('smoke, checkout')} value={labels} onChange={(e) => setLabels(e.target.value)} /></div>
        <div className="field"><label htmlFor="tm-issues">{t('Covers the issues')}</label><input id="tm-issues" className="input" placeholder={`${project.key}-12, ${project.key}-15`} value={issues} onChange={(e) => setIssues(e.target.value)} /></div>
      </div>
      <div className="field"><label htmlFor="tm-pre">{t('Preconditions')}</label><textarea id="tm-pre" className="input" rows={2} value={preconditions} onChange={(e) => setPreconditions(e.target.value)} /><span className="field-hint">{t('Markdown works here and in the steps.')}</span></div>
      <div className="seg" role="group" aria-label={t('How it is written')} style={{ marginBottom: 10 }}>
        <button type="button" className={kind === 'steps' ? 'active' : ''} aria-pressed={kind === 'steps'} onClick={() => setKind('steps')}>{t('Steps')}</button>
        <button type="button" className={kind === 'scenario' ? 'active' : ''} aria-pressed={kind === 'scenario'} onClick={() => setKind('scenario')}>{t('Given / When / Then')}</button>
      </div>
      {kind === 'scenario' ? (
        <div className="field"><label htmlFor="tm-scenario">{t('Scenario')}</label><textarea id="tm-scenario" className="input tm-mono" rows={8} value={scenario} onChange={(e) => setScenario(e.target.value)} /><span className="field-hint">{t('Each line that starts with Given, When, Then, And or But is a step of its own in a run.')}</span></div>
      ) : (
        <>
          {steps.map((s, i) => (
            <div key={i} className="tm-step-edit">
              <span className="tm-step-n">{i + 1}</span>
              <textarea className="input" rows={2} aria-label={t('Step {n}', { n: i + 1 })} placeholder={t('What to do')} value={s.action} onChange={(e) => patch(i, { action: e.target.value })} />
              <textarea className="input" rows={2} aria-label={t('Expected result of step {n}', { n: i + 1 })} placeholder={t('What you expect')} value={s.expected} onChange={(e) => patch(i, { expected: e.target.value })} />
              <span className="tm-step-btns">
                <button type="button" className="icon-btn icon-btn-sm" disabled={i === 0} aria-label={t('Move step {n} up', { n: i + 1 })} onClick={() => move(i, -1)}>↑</button>
                <button type="button" className="icon-btn icon-btn-sm" aria-label={t('Remove step {n}', { n: i + 1 })} onClick={() => setSteps((all) => all.filter((_, n) => n !== i))}><Trash size={13} /></button>
              </span>
            </div>
          ))}
          <button type="button" className="btn btn-sm" onClick={() => setSteps((all) => [...all, { action: '', expected: '' }])}><Plus size={14} /> {t('Add a step')}</button>
        </>
      )}
      {existing && <p className="field-hint">{t('Changing the title, the preconditions or the steps makes version {n}; runs keep the version they executed.', { n: existing.version + 1 })}</p>}
      {err && <p className="error-text" role="alert">{err}</p>}
    </Modal>
  )
}

function ImportDialog({ project, onClose }: { project: Project; onClose: () => void }) {
  const [csv, setCsv] = useState('')
  const [result, setResult] = useState<{ dryRun: boolean; created: number; updated: number; errors: { row: number; message: string }[] } | null>(null)
  const [err, setErr] = useState('')
  const send = async (text: string, dryRun: boolean) => {
    setErr('')
    try { setResult(await api.post(`/projects/${project.key}/tests/import`, { csv: text, dryRun })) } catch (e) { setErr((e as Error).message) }
  }
  return (
    <Modal title={t('Import test cases')} onClose={onClose} width={560} footer={<><button className="btn" onClick={onClose}>{t('Close')}</button><button className="btn btn-primary" disabled={!result?.dryRun || result.created + result.updated === 0} onClick={() => send(csv, false)}>{t('Import')}</button></>}>
      <p className="muted small" style={{ marginTop: 0 }}>{t('A CSV file with a Title column and, as you like, Key, Folder, Priority, Labels, Preconditions, Steps (one per line, “do this => expect that”), Scenario and Covers — the columns of the export. Rows with the key of a test case update it.')}</p>
      <label className="btn btn-sm">{t('Choose a CSV file…')}<input type="file" accept=".csv,text/csv" hidden onChange={async (e) => { const f = e.target.files?.[0]; if (f) { const text = await f.text(); setCsv(text); void send(text, true) } }} /></label>
      {err && <p className="error-text" role="alert">{err}</p>}
      {result && (
        <>
          <p role="status" className="small">{result.dryRun ? t('Checked: {created} would be added, {updated} updated.', { created: result.created, updated: result.updated }) : t('Imported: {created} added, {updated} updated.', { created: result.created, updated: result.updated })}</p>
          {result.errors.length > 0 && <ul className="small error-text">{result.errors.slice(0, 20).map((e) => <li key={e.row}>{t('Row {row}: {message}', { row: e.row, message: e.message })}</li>)}</ul>}
        </>
      )}
    </Modal>
  )
}

// ---- plans and run templates --------------------------------------------------------------------------

function PlansTab({ project }: { project: Project }) {
  const qc = useQueryClient()
  const toast = useToast()
  const q = useQuery({ queryKey: ['tests', project.key, 'plans'], queryFn: () => api.get<{ plans: Plan[]; templates: Template[] }>(`/projects/${project.key}/test-plans`) })
  const cases = useQuery({ queryKey: ['tests', project.key, 'cases', '', '', '', false], queryFn: () => api.get<CaseList>(`/projects/${project.key}/tests`) })
  const [editing, setEditing] = useState<Plan | 'new' | null>(null)
  const [starting, setStarting] = useState<{ plan?: Plan; template?: Template } | null>(null)
  const refresh = () => qc.invalidateQueries({ queryKey: ['tests', project.key] })
  const can = cases.data?.canManage
  if (q.isLoading) return <Loading />
  if (q.error) return <ErrorBox error={q.error} />
  const d = q.data!
  return (
    <>
      <div className="row" style={{ marginBottom: 10 }}>
        <p className="muted small" style={{ margin: 0 }}>{t('A plan is a named set of test cases — picked by hand, by a filter, or both — optionally for a release. Start a run from it whenever it should be checked.')}</p>
        <span className="spacer" />
        {can && <button className="btn btn-primary btn-sm" onClick={() => setEditing('new')}><Plus size={14} /> {t('New test plan')}</button>}
      </div>
      {d.plans.length === 0 && <Empty icon={<FlaskConical size={32} />} title={t('No test plans yet')} />}
      {d.plans.length > 0 && (
        <table className="list tm-table">
          <thead><tr><th>{t('Plan')}</th><th>{t('Release')}</th><th>{t('Test cases')}</th><th /></tr></thead>
          <tbody>
            {d.plans.map((p) => (
              <tr key={p.id}>
                <td>{can ? <button className="link-btn tm-title" onClick={async () => setEditing(await api.get<Plan>(`/test-plans/${p.id}`))}>{p.name}</button> : p.name}{p.description && <div className="small faint">{p.description}</div>}</td>
                <td className="small">{p.version?.name ?? <span className="faint">—</span>}</td>
                <td className="small">{p.caseCount}{p.filter && <span className="faint"> · {t('by filter')}</span>}</td>
                <td style={{ textAlign: 'end' }}>
                  {cases.data?.canExecute && <button className="btn btn-sm" disabled={p.caseCount === 0} onClick={() => setStarting({ plan: p })}><Play size={13} /> {t('Start a run')}</button>}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {d.templates.length > 0 && (
        <>
          <h2 className="tm-h2">{t('Run templates')}</h2>
          <ul className="tm-plain">
            {d.templates.map((x) => (
              <li key={x.id} className="row" style={{ gap: 8 }}>
                <strong>{x.name}</strong> <span className="small faint">{x.plan.name}{x.environment && <> · {x.environment}</>}{x.testerIds.length > 0 && <> · {t('{count} testers', { count: x.testerIds.length })}</>}</span>
                <span className="spacer" />
                {cases.data?.canExecute && <button className="btn btn-sm" onClick={() => setStarting({ template: x })}><Play size={13} /> {t('Start a run')}</button>}
                {can && <button className="icon-btn icon-btn-sm" aria-label={t('Delete {name}', { name: x.name })} onClick={async () => { try { await api.del(`/test-run-templates/${x.id}`) } catch (e) { toast((e as Error).message) } refresh() }}><Trash size={13} /></button>}
              </li>
            ))}
          </ul>
        </>
      )}
      {editing && cases.data && <PlanDialog project={project} plan={editing === 'new' ? undefined : editing} cases={cases.data} onClose={() => { setEditing(null); refresh() }} />}
      {starting && <StartRunDialog project={project} {...starting} canManage={!!can} onClose={() => { setStarting(null); refresh() }} />}
    </>
  )
}

function PlanDialog({ project, plan, cases, onClose }: { project: Project; plan?: Plan; cases: CaseList; onClose: () => void }) {
  const [name, setName] = useState(plan?.name ?? '')
  const [description, setDescription] = useState(plan?.description ?? '')
  const [versionId, setVersionId] = useState(plan?.version?.id ?? '')
  const [picked, setPicked] = useState<string[]>(plan?.caseIds ?? [])
  const [folderId, setFolderId] = useState(plan?.filter?.folderId ?? '')
  const [labels, setLabels] = useState((plan?.filter?.labels ?? []).join(', '))
  const [priority, setPriority] = useState(plan?.filter?.priority ?? '')
  const [err, setErr] = useState('')
  const folders = folderOptions(cases.folders)
  const save = async () => {
    const filter = { folderId: folderId || null, labels: labels.split(',').map((l) => l.trim()).filter(Boolean), priority: priority || null }
    const body = { name: name.trim(), description: description.trim() || null, versionId: versionId || null, caseIds: picked, filter: filter.folderId || filter.labels.length || filter.priority ? filter : null }
    try {
      if (plan) await api.patch(`/test-plans/${plan.id}`, body)
      else await api.post(`/projects/${project.key}/test-plans`, body)
      onClose()
    } catch (e) { setErr((e as Error).message) }
  }
  return (
    <Modal title={plan ? t('Edit {name}', { name: plan.name }) : t('New test plan')} onClose={onClose} width={680}
      footer={<>
        {plan && <button className="btn btn-danger" onClick={async () => { if (window.confirm(t('Delete the plan {name}? Its runs stay.', { name: plan.name }))) { await api.del(`/test-plans/${plan.id}`); onClose() } }}>{t('Delete')}</button>}
        <span className="spacer" /><button className="btn" onClick={onClose}>{t('Cancel')}</button><button className="btn btn-primary" disabled={!name.trim()} onClick={save}>{plan ? t('Save') : t('Create plan')}</button></>}>
      <div className="ap-grid">
        <div className="field"><label htmlFor="tp-name">{t('Name')}</label><input id="tp-name" className="input" autoFocus maxLength={120} placeholder={t('e.g. Regression')} value={name} onChange={(e) => setName(e.target.value)} /></div>
        <div className="field"><label htmlFor="tp-version">{t('Release')}</label>
          <select id="tp-version" className="select" value={versionId} onChange={(e) => setVersionId(e.target.value)}><option value="">{t('None')}</option>{project.versions.filter((v) => v.status !== 'archived' || v.id === versionId).map((v) => <option key={v.id} value={v.id}>{v.name}</option>)}</select></div>
      </div>
      <div className="field"><label htmlFor="tp-desc">{t('Description')}</label><input id="tp-desc" className="input" value={description} onChange={(e) => setDescription(e.target.value)} /></div>
      <fieldset className="ps-fieldset">
        <legend>{t('Test cases by a filter')}</legend>
        <div className="row-wrap" style={{ gap: 8 }}>
          <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Folder')} value={folderId} onChange={(e) => setFolderId(e.target.value)}><option value="">{t('Any folder')}</option>{folders.map((f) => <option key={f.id} value={f.id}>{f.label}</option>)}</select>
          <input className="input input-sm" style={{ width: 180 }} aria-label={t('Labels')} placeholder={t('Labels: smoke, …')} value={labels} onChange={(e) => setLabels(e.target.value)} />
          <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Priority')} value={priority} onChange={(e) => setPriority(e.target.value)}><option value="">{t('Any priority')}</option>{['critical', 'high', 'medium', 'low'].map((x) => <option key={x} value={x}>{priorityLabel(x)}</option>)}</select>
        </div>
        <p className="field-hint">{t('Cases that match when a run starts are part of it — also ones written later.')}</p>
      </fieldset>
      <fieldset className="ps-fieldset">
        <legend>{t('Test cases picked by hand ({count})', { count: picked.length })}</legend>
        <div className="tm-pick">
          {cases.cases.map((c) => (
            <label key={c.id} className="row small tm-check"><input type="checkbox" checked={picked.includes(c.id)} onChange={(e) => setPicked((all) => (e.target.checked ? [...all, c.id] : all.filter((x) => x !== c.id)))} /> <span className="issue-key">{c.key}</span> {c.title}</label>
          ))}
          {cases.cases.length === 0 && <span className="faint small">{t('No test cases here yet')}</span>}
        </div>
      </fieldset>
      {err && <p className="error-text" role="alert">{err}</p>}
    </Modal>
  )
}

function StartRunDialog({ project, plan, template, canManage, onClose }: { project: Project; plan?: Plan; template?: Template; canManage: boolean; onClose: () => void }) {
  const nav = useNavigate()
  const people = usePeople().data ?? []
  const [name, setName] = useState('')
  const [environment, setEnvironment] = useState(template?.environment ?? '')
  const [testers, setTesters] = useState<string[]>(template?.testerIds ?? [])
  const [saveAs, setSaveAs] = useState('')
  const [err, setErr] = useState('')
  const start = async () => {
    try {
      const planId = plan?.id ?? template?.plan.id
      if (saveAs.trim() && planId) await api.post(`/projects/${project.key}/test-run-templates`, { name: saveAs.trim(), planId, environment: environment.trim() || null, testerIds: testers })
      const r = await api.post<{ id: string }>(`/projects/${project.key}/test-runs`, { name: name.trim() || undefined, planId, environment: environment.trim() || null, testerIds: testers })
      nav(`/projects/${project.key}/tests/runs/${r.id}`)
    } catch (e) { setErr((e as Error).message) }
  }
  return (
    <Modal title={t('Start a run of {name}', { name: plan?.name ?? template?.name ?? '' })} onClose={onClose} width={520}
      footer={<><button className="btn" onClick={onClose}>{t('Cancel')}</button><button className="btn btn-primary" onClick={start}><Play size={14} /> {t('Start the run')}</button></>}>
      <div className="field"><label htmlFor="tr-name">{t('Name of the run')}</label><input id="tr-name" className="input" autoFocus placeholder={t('default: the plan and today’s date')} value={name} onChange={(e) => setName(e.target.value)} /></div>
      <div className="field"><label htmlFor="tr-env">{t('Environment')}</label><input id="tr-env" className="input" placeholder={t('e.g. Staging, Firefox 140')} value={environment} onChange={(e) => setEnvironment(e.target.value)} /></div>
      <fieldset className="ps-fieldset">
        <legend>{t('Testers')}</legend>
        <div className="tm-pick" style={{ maxHeight: 150 }}>
          {people.map((p) => <label key={p.id} className="row small tm-check"><input type="checkbox" checked={testers.includes(p.id)} onChange={(e) => setTesters((all) => (e.target.checked ? [...all, p.id] : all.filter((x) => x !== p.id)))} /> {p.name}</label>)}
        </div>
      </fieldset>
      {canManage && !template && <div className="field"><label htmlFor="tr-save">{t('Keep as a run template named')}</label><input id="tr-save" className="input" placeholder={t('optional: the plan with this environment and these testers')} value={saveAs} onChange={(e) => setSaveAs(e.target.value)} /></div>}
      {err && <p className="error-text" role="alert">{err}</p>}
    </Modal>
  )
}

// ---- runs ---------------------------------------------------------------------------------------------

export function RunTable({ projectKey, runs }: { projectKey: string; runs: RunLine[] }) {
  return (
    <table className="list tm-table">
      <thead><tr><th>{t('Run')}</th><th>{t('Release')}</th><th>{t('Environment')}</th><th>{t('Progress')}</th><th>{t('State')}</th></tr></thead>
      <tbody>
        {runs.map((r) => (
          <tr key={r.id}>
            <td><Link to={`/projects/${projectKey}/tests/runs/${r.id}`}><span className="issue-key">{r.key}</span> {r.name}</Link></td>
            <td className="small">{r.version?.name ?? <span className="faint">—</span>}</td>
            <td className="small">{r.environment ?? <span className="faint">—</span>}</td>
            <td style={{ minWidth: 220 }}><RunProgress counts={r.counts} /></td>
            <td className="small">{r.finishedAt ? <>{t('Finished')} <span className="faint">{shortDate(r.finishedAt)}</span></> : <span className="badge badge-accent">{t('Running')}</span>}{r.passRate != null && <div className="faint">{t('{rate}% passed', { rate: r.passRate })}</div>}</td>
          </tr>
        ))}
      </tbody>
    </table>
  )
}

function RunsTab({ project }: { project: Project }) {
  const q = useQuery({ queryKey: ['tests', project.key, 'runs'], queryFn: () => api.get<RunLine[]>(`/projects/${project.key}/test-runs`) })
  if (q.isLoading) return <Loading />
  if (q.error) return <ErrorBox error={q.error} />
  if (q.data!.length === 0) return <Empty icon={<Play size={32} />} title={t('No test runs yet')}>{t('Start one from a test plan.')}</Empty>
  return <RunTable projectKey={project.key} runs={q.data!} />
}

// ---- coverage and reports -----------------------------------------------------------------------------

/** What the tests say about a set of issues: coverage numbers and the issues to look at. */
export function CoverageSummary({ report, projectKey }: { report: TestReport; projectKey: string }) {
  const c = report.coverage
  return (
    <>
      <div className="tm-tiles">
        <div className="tm-tile"><strong>{c.issues ? Math.round((c.covered / c.issues) * 100) : 0}%</strong><span>{t('{covered} of {total} issues have tests', { covered: c.covered, total: c.issues })}</span></div>
        <div className="tm-tile"><strong>{c.passing}</strong><span>{t('issues whose tests all passed')}</span></div>
        <div className={`tm-tile ${c.failingCount ? 'tm-bad' : ''}`}><strong>{c.failingCount}</strong><span>{t('issues with failing tests')}</span></div>
        <div className={`tm-tile ${report.failures.length ? 'tm-bad' : ''}`}><strong>{report.failures.length}</strong><span>{t('failures that stand open')}</span></div>
      </div>
      {report.failures.length > 0 && <>
        <h3 className="tm-h">{t('Open failures')}</h3>
        <ul className="tm-plain">
          {report.failures.map((f, i) => (
            <li key={i}><ResultBadge result={f.result} /> <span className="issue-key">{f.case?.key}</span> {f.case?.title} <span className="faint small">· <Link to={`/projects/${projectKey}/tests/runs/${f.run.id}`}>{f.run.name}</Link></span>
              {f.bugs.map((b) => <span key={b.key}> · <Link to={`/issues/${b.key}`} className="issue-key">{b.key}</Link> <span className="badge">{b.status.name}</span></span>)}
              {f.bugs.length === 0 && <span className="faint small"> · {t('no bug yet')}</span>}</li>
          ))}
        </ul>
      </>}
      {c.failing.length > 0 && <><h3 className="tm-h">{t('Issues with failing tests')}</h3><ul className="tm-plain">{c.failing.map((i) => <li key={i.key}><Link to={`/issues/${i.key}`} className="issue-key">{i.key}</Link> {i.title}</li>)}</ul></>}
      {c.withoutTests.length > 0 && <><h3 className="tm-h">{t('Issues without tests ({count})', { count: c.withoutTestsCount })}</h3><ul className="tm-plain">{c.withoutTests.slice(0, 50).map((i) => <li key={i.key}><Link to={`/issues/${i.key}`} className="issue-key">{i.key}</Link> {i.title}</li>)}</ul></>}
    </>
  )
}

function ReportTab({ project }: { project: Project }) {
  const [version, setVersion] = useState('')
  const q = useQuery({ queryKey: ['tests', project.key, 'report', version], queryFn: () => api.get<TestReport>(`/projects/${project.key}/test-report${qs({ version: version || null })}`) })
  // only the days on which something changed
  const days = (q.data?.history ?? []).filter((d, i, all) => i === 0 || JSON.stringify({ ...d, date: '' }) !== JSON.stringify({ ...all[i - 1], date: '' })).filter((d) => d.passed + d.failed + d.blocked + d.skipped > 0)
  return (
    <>
      <div className="row" style={{ gap: 8, marginBottom: 12 }}>
        <label htmlFor="tm-report-version" className="small">{t('Release')}</label>
        <select id="tm-report-version" className="select input-sm" style={{ width: 'auto' }} value={version} onChange={(e) => setVersion(e.target.value)}>
          <option value="">{t('All open issues of the project')}</option>
          {project.versions.map((v) => <option key={v.id} value={v.id}>{v.name}</option>)}
        </select>
      </div>
      {q.isLoading && <Loading />}
      {q.error && <ErrorBox error={q.error} />}
      {q.data && (
        <>
          <CoverageSummary report={q.data} projectKey={project.key} />
          <h2 className="tm-h2">{t('Runs and their pass rates')}</h2>
          {q.data.runs.length === 0 ? <p className="faint small">{t('No test runs yet')}</p> : <RunTable projectKey={project.key} runs={q.data.runs} />}
          <h2 className="tm-h2">{t('Where the test cases stood, day by day')}</h2>
          {days.length === 0 ? <p className="faint small">{t('No results in the last 30 days.')}</p> : (
            <table className="list tm-table" style={{ maxWidth: 560 }}>
              <thead><tr><th>{t('Day')}</th><th>{t('Passed')}</th><th>{t('Failed')}</th><th>{t('Blocked')}</th><th>{t('Skipped')}</th></tr></thead>
              <tbody>{days.slice(-14).map((d) => <tr key={d.date}><td>{shortDate(d.date)}</td><td>{d.passed}</td><td>{d.failed}</td><td>{d.blocked}</td><td>{d.skipped}</td></tr>)}</tbody>
            </table>
          )}
          <h2 className="tm-h2">{t('Test cases that keep flipping')}</h2>
          {q.data.flaky.length === 0 ? <p className="faint small">{t('None: no test case went back and forth between passed and failed.')}</p> : (
            <ul className="tm-plain">{q.data.flaky.map((f) => <li key={f.case.id}><span className="issue-key">{f.case.key}</span> {f.case.title} <span className="faint small">· {t('flipped {count} times', { count: f.flips })} · {f.results.map((r) => resultLabel(r)).join(' → ')}</span></li>)}</ul>
          )}
        </>
      )}
    </>
  )
}
