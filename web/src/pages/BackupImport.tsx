// Administration → Import: a backup from another tracker (its XML site backup, a .zip) uploaded,
// checked by the server, the projects and people decided on, then imported in the background.
import { t, locale } from '../i18n'
import { useEffect, useRef, useState, type DragEvent } from 'react'
import { Link } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { ArchiveRestore, CircleCheck, CircleX, FileArchive, FolderKanban, RotateCcw, TriangleAlert, Upload } from 'lucide-react'
import { api, ago, ApiError, dateTime } from '../api'
import { Confirm, ErrorBox, Loading, Spinner, useProjects, useToast } from '../ui'
import './BackupImport.css'

interface Warning {
  code: string
  message: string
  params: Record<string, string>
  count: number
}

interface ProjectSummary {
  key: string
  name: string
  formerKeys: string[]
  issues: number
  comments: number
  worklogs: number
  attachments: number
  missingFiles: number
  links: number
  sprints: number
  history: number
  existing: { key: string; name: string; issues: number } | null
  imported: number
  validKey: boolean
}

interface Summary {
  projects: ProjectSummary[]
  people: { total: number; matched: number; active: number; inactive: number }
  files: number
  hasSprints: boolean
  warnings: Warning[]
}

interface Count {
  imported: number
  existing: number
}

interface ProjectReport {
  source: string
  key: string
  name: string
  id: string | null
  created: boolean
  skipped: boolean
  counts: Record<string, Count>
}

interface Report {
  dryRun: boolean
  people: Record<string, number>
  projects: ProjectReport[]
  warnings: Warning[]
}

interface Progress {
  phase: string
  project: string | null
  done: number
  total: number
}

type Choice = { action: 'new'; key: string } | { action: 'into'; key: string } | { action: 'skip' }
type People = 'invite' | 'deactivated'

interface Job {
  id: string
  fileName: string
  size: number
  status: 'checking' | 'checked' | 'running' | 'done' | 'failed'
  summary: Summary | null
  options: { projects: Record<string, Choice>; people: People; dryRun: boolean } | null
  progress: Progress | null
  report: Report | null
  error: string | null
  fileKept: boolean
  createdAt: string
  finishedAt: string | null
  expiresAt: string
}

interface Jobs {
  imports: Job[]
  maxUploadMb: number
}

const busy = (j: Job | undefined) => j?.status === 'checking' || j?.status === 'running'

function bytes(n: number): string {
  const mb = n / (1024 * 1024)
  return new Intl.NumberFormat(locale(), { style: 'unit', unit: mb >= 1 ? 'megabyte' : 'kilobyte', maximumFractionDigits: 1 }).format(mb >= 1 ? mb : n / 1024)
}

/** A warning of the server in the reader's language. */
function warningText(w: Warning): string {
  const p = w.params
  switch (w.code) {
    case 'people-unlisted': return t('People the backup names but doesn’t list (removed from the other tracker) become deactivated accounts')
    case 'sprints-no-dates': return t('Sprints without their dates in the backup are imported as future sprints')
    case 'no-sprint-dates': return t('The backup has no activeobjects.xml: sprints come without their dates')
    case 'number-taken': return t('{key} is taken in {project}: the issue gets a new number (its old key keeps working)', { key: p.key, project: p.project })
    case 'resolved-not-done': return t('Issues in “{status}” with a resolution are imported as “{done}” (done)', { status: p.status, done: p.done })
    case 'several-components': return t('Issues with several components keep the first one')
    case 'several-versions': return t('Issues with several fix versions keep the first one')
    case 'missing-file': return t('Attachments whose file isn’t in the backup are left out')
    case 'file-too-large': return t('Attachments larger than the upload limit ({mb} MB) are left out', { mb: p.mb })
    case 'unreadable-file': return t('An attachment couldn’t be read: {error}', { error: p.error })
    case 'empty-comment': return t('Empty comments are left out')
    case 'restricted-comments': return t('Comments only a group or role could see are visible to everyone who can see the project')
    case 'link-type': return t('Links of the type “{name}” are imported as “relates to”', { name: p.name })
    case 'subtask-parent': return t('Sub-tasks can’t have children here: such parent links are left out')
    case 'parent-cycle': return t('Parent links that would go round in a circle are left out')
    case 'links-outside': return t('Links to issues of projects that aren’t imported are left out')
    case 'affects-versions': return t('Affected versions aren’t kept (an issue has one release here: its fix version)')
    case 'other-fields': return t('Values of other custom fields aren’t imported')
    default: return w.message
  }
}

function Warnings({ list }: { list: Warning[] }) {
  if (!list.length) return null
  return (
    <div className="bi-warnings" role="note">
      <h3><TriangleAlert size={15} /> {t('Good to know')}</h3>
      <ul>
        {list.map((w, i) => (
          <li key={i}>{warningText(w)}{w.count > 1 && <span className="faint"> · {t('{count} times', { count: w.count })}</span>}</li>
        ))}
      </ul>
    </div>
  )
}

export function BackupImportTab() {
  const qc = useQueryClient()
  const [another, setAnother] = useState(false)
  const jobs = useQuery({
    queryKey: ['admin', 'backup-imports'],
    queryFn: () => api.get<Jobs>('/admin/backup-imports'),
    refetchInterval: (q) => (busy(q.state.data?.imports[0]) ? 1000 : false),
  })
  if (jobs.isLoading) return <Loading />
  if (jobs.error) return <ErrorBox error={jobs.error} />
  const list = jobs.data?.imports ?? []
  const current = list[0]
  const refresh = () => qc.invalidateQueries({ queryKey: ['admin', 'backup-imports'] })
  // the latest upload stays on screen (also after a reload) until it's done with
  const showCurrent = current && !another && (busy(current) || current.status === 'checked' || current.status === 'failed' || current.status === 'done')
  return (
    <div className="bi">
      <p className="adm-intro muted">
        {t('Moving from another tracker? Download its XML site backup (a .zip with entities.xml and the attachments) and upload it here. TaskCraft checks it and shows what it holds; nothing changes until you start the import. People, projects, issues with their numbers, comments, time logs, attachments, links, watchers, components, releases, sprints and the history the reports need come along. Nobody gets an e-mail.')}
      </p>
      {showCurrent ? (
        <CurrentImport job={current} onChanged={refresh} onAnother={() => setAnother(true)} />
      ) : (
        <UploadCard maxMb={jobs.data?.maxUploadMb ?? 256} onUploaded={() => { setAnother(false); refresh() }} />
      )}
      {list.length > (showCurrent ? 1 : 0) && <EarlierImports jobs={showCurrent ? list.slice(1) : list} />}
    </div>
  )
}

function UploadCard({ maxMb, onUploaded }: { maxMb: number; onUploaded: () => void }) {
  const [file, setFile] = useState<File | null>(null)
  const [sent, setSent] = useState<number | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [over, setOver] = useState(false)
  const xhr = useRef<XMLHttpRequest | null>(null)
  useEffect(() => () => xhr.current?.abort(), [])
  const max = maxMb * 1024 * 1024
  const choose = (f: File | undefined) => {
    setError(null)
    if (!f) return
    if (!/\.zip$/i.test(f.name)) {
      setError(t('Choose the backup’s .zip file.'))
      return
    }
    if (f.size > max) {
      setError(t('{name} is {size}: uploads here take up to {max}. Raise TASKCRAFT_MAX_UPLOAD_MB or import it on the server with “taskcraft import-backup”.', { name: f.name, size: bytes(f.size), max: bytes(max) }))
      return
    }
    setFile(f)
  }
  const upload = () => {
    if (!file) return
    const r = new XMLHttpRequest()
    xhr.current = r
    r.open('POST', `/api/admin/backup-imports?name=${encodeURIComponent(file.name)}`)
    r.setRequestHeader('x-taskcraft', '1')
    r.setRequestHeader('content-type', 'application/zip')
    r.upload.onprogress = (e) => setSent(e.lengthComputable ? e.loaded / e.total : null)
    r.onload = () => {
      xhr.current = null
      if (r.status >= 200 && r.status < 300) {
        setFile(null)
        setSent(null)
        onUploaded()
      } else {
        setSent(null)
        let msg = r.statusText
        try {
          msg = JSON.parse(r.responseText).message ?? msg
        } catch { /* keep the status text */ }
        setError(r.status === 413 ? t('The file is larger than the upload limit ({max}).', { max: bytes(max) }) : msg)
      }
    }
    r.onerror = () => {
      xhr.current = null
      setSent(null)
      setError(t('The upload broke off. Check the connection and try again.'))
    }
    setSent(0)
    setError(null)
    r.send(file)
  }
  const uploading = sent !== null
  const drop = (e: DragEvent) => {
    e.preventDefault()
    setOver(false)
    if (!uploading) choose(e.dataTransfer.files?.[0])
  }
  return (
    <div className="adm-card bi-card">
      <div className="adm-card-head">
        <h2><ArchiveRestore size={16} /> {t('Import a backup from another tracker')}</h2>
        <p className="muted small">{t('An XML site backup (.zip), up to {max}.', { max: bytes(max) })}</p>
      </div>
      <div className="adm-card-body">
        <label
          className={`dropzone bi-drop ${over ? 'over' : ''}`}
          onDragOver={(e) => { e.preventDefault(); setOver(true) }}
          onDragLeave={() => setOver(false)}
          onDrop={drop}
        >
          <FileArchive size={20} />
          <span>{file ? t('{name} ({size}) · choose another file', { name: file.name, size: bytes(file.size) }) : t('Choose the .zip file or drop it here')}</span>
          <input className="bi-file" type="file" accept=".zip,application/zip" disabled={uploading} onChange={(e) => choose(e.target.files?.[0])} />
        </label>
        {error && <p className="error-text" role="alert">{error}</p>}
        {uploading && <Bar value={sent} label={t('Uploading…')} />}
      </div>
      <div className="adm-card-foot">
        {uploading && <button className="btn" onClick={() => { xhr.current?.abort(); xhr.current = null; setSent(null) }}>{t('Cancel')}</button>}
        <button className="btn btn-primary" disabled={!file || uploading} onClick={upload}><Upload size={14} /> {t('Upload and check')}</button>
      </div>
    </div>
  )
}

/** A progress bar (`value` 0…1, or unknown). */
function Bar({ value, label }: { value: number | null; label: string }) {
  const pct = value === null ? null : Math.round(Math.min(1, Math.max(0, value)) * 100)
  return (
    <div className="bi-progress">
      <div className="row small"><span>{label}</span>{pct !== null && <span className="faint">{pct} %</span>}</div>
      <div className={`bi-bar ${pct === null ? 'unknown' : ''}`} role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct ?? undefined}>
        <span style={{ width: pct === null ? undefined : `${pct}%` }} />
      </div>
    </div>
  )
}

function phaseText(p: Progress | null, checking: boolean): string {
  if (!p || p.phase === 'reading') return checking ? t('Reading and checking the backup…') : t('Reading the backup…')
  switch (p.phase) {
    case 'people': return t('Matching people…')
    case 'issues': return t('Importing {project}: issue {done} of {total}', { project: p.project ?? '', done: p.done + 1, total: p.total })
    case 'links': return t('Connecting parents and links…')
    case 'saving': return t('Saving…')
    default: return t('Working…')
  }
}

function CurrentImport({ job, onChanged, onAnother }: { job: Job; onChanged: () => void; onAnother: () => void }) {
  const [discarding, setDiscarding] = useState(false)
  const head = (
    <div className="adm-card-head bi-head">
      <h2><FileArchive size={16} /> {job.fileName}</h2>
      <p className="muted small">{bytes(job.size)} · {t('uploaded {when}', { when: ago(job.createdAt) })}</p>
    </div>
  )
  if (job.status === 'checking' || job.status === 'running') {
    const p = job.progress
    const value = p?.phase === 'issues' && p.total ? p.done / p.total : null
    return (
      <div className="adm-card bi-card" aria-busy="true">
        {head}
        <div className="adm-card-body" aria-live="polite">
          <Bar value={value} label={phaseText(p, job.status === 'checking')} />
          <p className="faint small">{job.status === 'running' ? t('This can take a while for big backups. You can leave this page; the import goes on.') : t('This takes a moment.')}</p>
        </div>
      </div>
    )
  }
  return (
    <>
      {job.status === 'failed' && (
        <div className="adm-card bi-card">
          {head}
          <div className="adm-card-body">
            <p className="bi-failed" role="alert"><CircleX size={16} /> {job.summary ? t('The import stopped; nothing was changed.') : t('The backup couldn’t be read.')}</p>
            {job.error && <pre className="bi-error">{job.error}</pre>}
          </div>
          <div className="adm-card-foot">
            <button className="btn" onClick={onAnother}><RotateCcw size={14} /> {t('Upload another backup')}</button>
          </div>
        </div>
      )}
      {job.status === 'done' && job.report && <ReportCard job={job} report={job.report} onAnother={onAnother} />}
      {job.summary && job.fileKept && (job.status === 'checked' || job.status === 'failed' || job.report?.dryRun) && (
        <Choices key={job.id} job={job} summary={job.summary} onStarted={onChanged} onDiscard={() => setDiscarding(true)} />
      )}
      {discarding && (
        <Confirm
          title={t('Discard this backup?')}
          body={t('The uploaded file is deleted. Nothing was imported from it.')}
          confirmLabel={t('Discard')}
          danger
          onConfirm={async () => {
            await api.del(`/admin/backup-imports/${job.id}`)
            onChanged()
          }}
          onClose={() => setDiscarding(false)}
        />
      )}
    </>
  )
}

function Choices({ job, summary, onStarted, onDiscard }: { job: Job; summary: Summary; onStarted: () => void; onDiscard: () => void }) {
  const toast = useToast()
  const projects = useProjects().data ?? []
  const open = projects.filter((p) => !p.archivedAt)
  const initial = (): Record<string, Choice> => {
    const out: Record<string, Choice> = {}
    for (const p of summary.projects) {
      const before = job.options?.projects?.[p.key]
      if (before) out[p.key] = before
      // a project with the same key here: into it (the backup's earlier import, or the same team)
      else if (p.existing) out[p.key] = { action: 'into', key: p.existing.key }
      else out[p.key] = { action: 'new', key: p.validKey ? p.key : '' }
    }
    return out
  }
  const [choices, setChoices] = useState<Record<string, Choice>>(initial)
  const [people, setPeople] = useState<People>(job.options?.people ?? 'invite')
  const [starting, setStarting] = useState(false)
  const nf = new Intl.NumberFormat(locale())
  const taken = new Set(open.map((p) => p.key.toUpperCase()))
  const problems: string[] = []
  const newKeys = new Set<string>()
  for (const p of summary.projects) {
    const c = choices[p.key]
    if (c?.action === 'new') {
      const k = c.key.trim().toUpperCase()
      if (!/^[A-Z][A-Z0-9]{1,9}$/.test(k)) problems.push(t('{key}: a project key has 2–10 letters or digits and starts with a letter.', { key: p.key }))
      else if (taken.has(k)) problems.push(t('{key}: the key {new} is taken; choose another or import into that project.', { key: p.key, new: k }))
      else if (newKeys.has(k)) problems.push(t('{key}: two projects can’t get the same new key.', { key: p.key }))
      newKeys.add(k)
    }
    if (c?.action === 'into' && !c.key) problems.push(t('{key}: choose the project to import into.', { key: p.key }))
  }
  const chosen = summary.projects.filter((p) => choices[p.key]?.action !== 'skip')
  if (!chosen.length) problems.push(t('Choose at least one project to import.'))
  const issues = chosen.reduce((n, p) => n + p.issues - p.imported, 0)
  const start = async (dryRun: boolean) => {
    setStarting(true)
    try {
      const body = Object.fromEntries(Object.entries(choices).map(([k, c]) => [k, c.action === 'skip' ? c : { ...c, key: c.key.trim().toUpperCase() }]))
      await api.post(`/admin/backup-imports/${job.id}/run`, { projects: body, people, dryRun })
      onStarted()
    } catch (e) {
      toast(e instanceof ApiError ? e.message : String(e))
    } finally {
      setStarting(false)
    }
  }
  const set = (key: string, c: Choice) => setChoices((all) => ({ ...all, [key]: c }))
  return (
    <div className="adm-card bi-card">
      <div className="adm-card-head">
        <h2>{t('What the backup holds')}</h2>
        <p className="muted small">
          {t('{count} people are named in it; {matched} of them have an account here already (same e-mail address or name).', { count: summary.people.total, matched: summary.people.matched })}
          {' '}{t('{count} attachment files.', { count: summary.files })}
        </p>
      </div>
      <div className="adm-card-body">
        <div className="adm-table-wrap">
          <table className="list adm-table bi-table">
            <caption className="sr-only">{t('Projects in the backup')}</caption>
            <thead>
              <tr>
                <th scope="col">{t('Project')}</th>
                <th scope="col" className="num">{t('Issues')}</th>
                <th scope="col" className="num">{t('Comments')}</th>
                <th scope="col" className="num">{t('Time logs')}</th>
                <th scope="col" className="num">{t('Attachments')}</th>
                <th scope="col" className="num">{t('Links')}</th>
                <th scope="col">{t('Import as')}</th>
              </tr>
            </thead>
            <tbody>
              {summary.projects.map((p) => {
                const c = choices[p.key] ?? { action: 'skip' }
                const id = `bi-${p.key}`
                return (
                  <tr key={p.key} className={c.action === 'skip' ? 'adm-off' : ''}>
                    <td>
                      <div className="bi-project"><span className="issue-key">{p.key}</span> <span>{p.name}</span></div>
                      {p.formerKeys.length > 0 && <div className="faint small">{t('formerly {keys}', { keys: p.formerKeys.join(', ') })}</div>}
                      {p.sprints > 0 && <div className="faint small">{t('{count} sprints', { count: p.sprints })}</div>}
                      {p.imported > 0 && <div className="small bi-note">{t('{count} issues are here from an earlier import and are skipped', { count: p.imported })}</div>}
                    </td>
                    <td className="num">{nf.format(p.issues)}</td>
                    <td className="num">{nf.format(p.comments)}</td>
                    <td className="num">{nf.format(p.worklogs)}</td>
                    <td className="num">
                      {nf.format(p.attachments)}
                      {p.missingFiles > 0 && <div className="faint small">{t('{count} without file', { count: p.missingFiles })}</div>}
                    </td>
                    <td className="num">{nf.format(p.links)}</td>
                    <td className="bi-choice">
                      <select
                        id={id}
                        className="select input-sm"
                        aria-label={t('Import {key} as', { key: p.key })}
                        value={c.action}
                        onChange={(e) => {
                          const a = e.target.value
                          if (a === 'skip') set(p.key, { action: 'skip' })
                          else if (a === 'new') set(p.key, { action: 'new', key: taken.has(p.key) || !p.validKey ? '' : p.key })
                          else set(p.key, { action: 'into', key: p.existing?.key ?? open[0]?.key ?? '' })
                        }}
                      >
                        <option value="new">{t('A new project')}</option>
                        <option value="into" disabled={!open.length}>{t('An existing project')}</option>
                        <option value="skip">{t('Leave it out')}</option>
                      </select>
                      {c.action === 'new' && (
                        <input
                          className="input input-sm bi-key"
                          aria-label={t('Key of the new project for {key}', { key: p.key })}
                          placeholder={t('Key')}
                          value={c.key}
                          maxLength={10}
                          onChange={(e) => set(p.key, { action: 'new', key: e.target.value.toUpperCase() })}
                        />
                      )}
                      {c.action === 'into' && (
                        <select className="select input-sm" aria-label={t('Project to import {key} into', { key: p.key })} value={c.key} onChange={(e) => set(p.key, { action: 'into', key: e.target.value })}>
                          {open.map((o) => <option key={o.id} value={o.key}>{o.key} · {o.name}</option>)}
                        </select>
                      )}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
        <fieldset className="bi-people">
          <legend>{t('People without an account here')}</legend>
          <label className="bi-radio">
            <input type="radio" name="bi-people" checked={people === 'invite'} onChange={() => setPeople('invite')} />
            <span>
              <strong>{t('Prepare invitations')}</strong>
              <span className="faint small">{t('Accounts waiting for an invitation, which you send from Users when you’re ready. People who are inactive in the backup get deactivated accounts.')}</span>
            </span>
          </label>
          <label className="bi-radio">
            <input type="radio" name="bi-people" checked={people === 'deactivated'} onChange={() => setPeople('deactivated')} />
            <span>
              <strong>{t('Keep them as deactivated accounts')}</strong>
              <span className="faint small">{t('Their names stay on their work; nobody can sign in with them. You can still invite people later.')}</span>
            </span>
          </label>
        </fieldset>
        <Warnings list={summary.warnings} />
        {problems.map((m, i) => <p key={i} className="error-text">{m}</p>)}
      </div>
      <div className="adm-card-foot">
        <span className="adm-foot-msg faint small">{t('Kept until {when}, then deleted.', { when: dateTime(job.expiresAt) })}</span>
        <button className="btn" onClick={onDiscard} disabled={starting}>{t('Discard')}</button>
        <button className="btn" onClick={() => start(true)} disabled={starting || problems.length > 0}>{t('Test the import')}</button>
        <button className="btn btn-primary" onClick={() => start(false)} disabled={starting || problems.length > 0}>
          {starting ? <Spinner size={14} /> : <Upload size={14} />} {t('Import {count} issues', { count: issues })}
        </button>
      </div>
    </div>
  )
}

const COUNTS: [string, () => string][] = [
  ['issues', () => t('Issues')],
  ['comments', () => t('Comments')],
  ['worklogs', () => t('Time logs')],
  ['attachments', () => t('Attachments')],
  ['links', () => t('Links')],
  ['parents', () => t('Parents')],
  ['watchers', () => t('Watchers')],
  ['labels', () => t('Labels')],
  ['components', () => t('Components')],
  ['versions', () => t('Releases')],
  ['sprints', () => t('Sprints')],
  ['statuses', () => t('New statuses')],
  ['types', () => t('New issue types')],
  ['history', () => t('History entries')],
]

function ReportCard({ job, report, onAnother }: { job: Job; report: Report; onAnother: () => void }) {
  const nf = new Intl.NumberFormat(locale())
  const p = (k: string) => report.people[k] ?? 0
  const shown = report.projects.filter((x) => !x.skipped)
  return (
    <div className="adm-card bi-card">
      <div className="adm-card-head">
        <h2 className="bi-done">
          <CircleCheck size={17} />
          {report.dryRun ? t('Test finished: nothing was changed') : t('Import finished')}
        </h2>
        <p className="muted small">
          {job.finishedAt && `${dateTime(job.finishedAt)} · `}
          {t('People: {matched} matched to accounts here, {invited} invitations prepared, {deactivated} deactivated accounts', { matched: p('matched'), invited: p('invited'), deactivated: p('deactivated') })}
        </p>
      </div>
      <div className="adm-card-body">
        <div className="bi-reports">
          {shown.map((r) => (
            <section key={r.source} className="bi-report">
              <h3>
                <FolderKanban size={15} />
                {report.dryRun || !r.id ? <span>{r.key}</span> : <Link to={`/projects/${r.key}`}>{r.key} · {r.name}</Link>}
                <span className="badge">{r.created ? t('new project') : t('existing project')}</span>
              </h3>
              {r.source !== r.key && <p className="faint small">{t('{old} in the backup', { old: r.source })}</p>}
              <dl className="bi-counts">
                {COUNTS.filter(([k]) => r.counts[k] && (r.counts[k].imported || r.counts[k].existing)).map(([k, label]) => (
                  <div key={k}>
                    <dt>{label()}</dt>
                    <dd>
                      {nf.format(r.counts[k].imported)}
                      {r.counts[k].existing > 0 && <span className="faint small"> · {t('{count} already here', { count: r.counts[k].existing })}</span>}
                    </dd>
                  </div>
                ))}
              </dl>
            </section>
          ))}
        </div>
        {report.projects.some((x) => x.skipped) && (
          <p className="faint small">{t('Left out: {keys}', { keys: report.projects.filter((x) => x.skipped).map((x) => x.source).join(', ') })}</p>
        )}
        <Warnings list={report.warnings} />
      </div>
      <div className="adm-card-foot">
        {!report.dryRun && p('invited') > 0 && <Link className="btn" to="/admin/users">{t('Send invitations')}</Link>}
        {!report.dryRun && <button className="btn" onClick={onAnother}><Upload size={14} /> {t('Import another backup')}</button>}
      </div>
    </div>
  )
}

function EarlierImports({ jobs }: { jobs: Job[] }) {
  const status = (j: Job) => {
    switch (j.status) {
      case 'done': return j.report?.dryRun ? t('tested') : t('imported')
      case 'failed': return t('failed')
      case 'checked': return t('checked, not imported')
      default: return t('in progress')
    }
  }
  return (
    <div className="bi-earlier">
      <h3>{t('Earlier uploads')}</h3>
      <ul>
        {jobs.map((j) => (
          <li key={j.id}>
            <FileArchive size={14} /> <span>{j.fileName}</span>
            <span className="faint small">{dateTime(j.createdAt)} · {status(j)}</span>
            {j.status === 'done' && !j.report?.dryRun && j.report?.projects.filter((p) => !p.skipped).map((p) => (
              <Link key={p.key} className="issue-key" to={`/projects/${p.key}`}>{p.key}</Link>
            ))}
          </li>
        ))}
      </ul>
    </div>
  )
}
