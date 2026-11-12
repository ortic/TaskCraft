// Administration → Import: from another tracker. Its XML site backup (a .zip) is uploaded, or
// TaskCraft connects to its API with a token and reads the projects chosen; either way the
// server checks what it got, the projects and people are decided on, then it's imported in the
// background. Tokens stay in this page's memory and are sent with the requests that need them.
import { t, locale } from '../i18n'
import { useEffect, useRef, useState, type DragEvent, type FormEvent } from 'react'
import { Link } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { ArchiveRestore, CircleCheck, CircleX, FileArchive, FolderKanban, ListChecks, Plug, Plus, RotateCcw, TriangleAlert, Upload, X } from 'lucide-react'
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
  suggestedKey?: string
  name: string
  formerKeys: string[]
  parent?: string | null
  issues: number
  comments: number
  worklogs: number
  attachments: number
  missingFiles: number
  links: number
  sprints: number
  history: number
  confidential?: number
  existing: { key: string; name: string; issues: number } | null
  imported: number
  validKey: boolean
}

type Markup = 'textile' | 'markdown'

interface Summary {
  source?: string
  projects: ProjectSummary[]
  people: { total: number; matched: number; active: number; inactive: number }
  files: number
  hasSprints: boolean
  markup?: Markup | null
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
  parent?: string | null
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
  until?: string | null
}

type Choice = { action: 'new'; key: string } | { action: 'into'; key: string } | { action: 'skip' }
type People = 'invite' | 'deactivated'

/** Where an import comes from: an uploaded backup or another tracker's API. */
type Source = 'xml-site-backup' | 'github' | 'gitlab' | 'redmine'
type ApiSource = Exclude<Source, 'xml-site-backup'>

interface Job {
  id: string
  source?: Source
  connection?: { baseUrl: string; projects: string[]; token: boolean; markup?: Markup | null } | null
  fileName: string
  size: number
  status: 'checking' | 'checked' | 'running' | 'done' | 'failed'
  summary: Summary | null
  options: { projects: Record<string, Choice>; people: People; dryRun: boolean; markup?: Markup | null } | null
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

interface Available {
  key: string
  name: string
  description: string | null
  issues: number | null
  private: boolean
  parent: string | null
}

/** The trackers an import can come from (names on this screen only). */
const SOURCES: { id: Source; label: () => string }[] = [
  { id: 'xml-site-backup', label: () => t('{tool} (XML backup)', { tool: 'Jira' }) },
  { id: 'github', label: () => 'GitHub' },
  { id: 'gitlab', label: () => 'GitLab' },
  { id: 'redmine', label: () => 'Redmine' },
]

const sourceName = (s: Source | undefined) => SOURCES.find((x) => x.id === s)?.label() ?? ''
const isApi = (s: Source | undefined): s is ApiSource => !!s && s !== 'xml-site-backup'

/** What the connect form asks for, per API source. */
function apiFields(s: ApiSource) {
  switch (s) {
    case 'github':
      return {
        url: t('Address'),
        urlHint: t('Leave it empty for github.com; for GitHub Enterprise, its address.'),
        urlPlaceholder: 'https://github.com',
        urlRequired: false,
        token: t('Personal access token'),
        tokenHint: t('Fine-grained or classic; reading is enough (issues and metadata). Public repositories can be read without one, but slowly.'),
        projects: t('Repositories'),
        projectPlaceholder: 'owner/repo',
        owner: t('Owner (user or organization)'),
        list: t('Show repositories'),
      }
    case 'gitlab':
      return {
        url: t('Address'),
        urlHint: t('Leave it empty for gitlab.com; for a self-managed GitLab, its address.'),
        urlPlaceholder: 'https://gitlab.com',
        urlRequired: false,
        token: t('Personal, group or project access token'),
        tokenHint: t('With the read_api scope. Public projects can be read without one (without confidential issues and internal notes).'),
        projects: t('Projects'),
        projectPlaceholder: 'group/project',
        owner: t('Group or user'),
        list: t('Show projects'),
      }
    case 'redmine':
      return {
        url: t('Address'),
        urlHint: t('The server’s address. Its REST API must be on (Administration → Settings → API).'),
        urlPlaceholder: 'https://redmine.example.com',
        urlRequired: true,
        token: t('API key'),
        tokenHint: t('Under My account → API access key. An administrator’s key also brings watchers, e-mail addresses, locked accounts and the definitions of custom fields.'),
        projects: t('Projects'),
        projectPlaceholder: t('project identifier'),
        owner: t('Parent project (optional)'),
        list: t('Show projects'),
      }
  }
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
    case 'people-unlisted': return t('People the source names but doesn’t list (removed from it) become deactivated accounts')
    case 'sprints-no-dates': return t('Sprints without their dates in the backup are imported as future sprints')
    case 'no-sprint-dates': return t('The backup has no activeobjects.xml: sprints come without their dates')
    case 'number-taken': return t('{key} is taken in {project}: the issue gets a new number (its old key keeps working)', { key: p.key, project: p.project })
    case 'resolved-not-done': return t('Issues in “{status}” with a resolution are imported as “{done}” (done)', { status: p.status, done: p.done })
    case 'several-components': return t('Issues with several components keep the first one')
    case 'several-versions': return t('Issues with several fix versions keep the first one')
    case 'missing-file': return t('Attachments whose file isn’t in the backup are left out')
    case 'missing-download': return t('Attachments whose file couldn’t be found in the source are left out')
    case 'file-too-large': return t('Attachments larger than the upload limit ({mb} MB) are left out', { mb: p.mb })
    case 'unreadable-file': return t('An attachment couldn’t be read: {error}', { error: p.error })
    case 'empty-comment': return t('Empty comments are left out')
    case 'restricted-comments': return t('Comments only a group or role could see are visible to everyone who can see the project')
    case 'confidential': return t('Confidential issues are visible to everyone who can see the project')
    case 'link-type': return t('Links of the type “{name}” are imported as “relates to”', { name: p.name })
    case 'subtask-parent': return t('Sub-tasks can’t have children here: such parent links are left out')
    case 'parent-cycle': return t('Parent links that would go round in a circle are left out')
    case 'links-outside': return t('Links to issues of projects that aren’t imported are left out')
    case 'affects-versions': return t('Affected versions aren’t kept (an issue has one release here: its fix version)')
    case 'other-fields': return t('Values of other custom fields aren’t imported')
    case 'field-value': return t('Values that don’t fit the field {name} are left out', { name: p.name })
    case 'no-token': return t('Without a token GitHub allows 60 requests an hour: reading more than a small repository takes long (the import waits for the limit)')
    case 'issues-off': return t('{project} has its issues turned off', { project: p.project })
    case 'pull-requests': return t('Pull requests aren’t imported, only issues')
    case 'no-token-gitlab': return t('Without a token only public projects can be read, and confidential issues and internal notes stay out')
    case 'time-subtracted': return t('Time subtracted in the source is left out: some issues show more time spent there')
    case 'notes-need-token': return t('The server shows comments, time logs and their history only with a token: they stay out')
    case 'no-key': return t('Without an API key only public projects can be read, without private issues and notes')
    case 'not-admin': return t('The API key isn’t an administrator’s: watchers, e-mail addresses, locked accounts and custom field definitions may be missing')
    case 'no-time-entries': return t('Time entries couldn’t be read (time tracking is off, or the API key can’t see them)')
    case 'time-without-issue': return t('Time logged on a project rather than an issue is left out')
    case 'time-elsewhere': return t('Time logged on issues of projects that aren’t read is left out')
    case 'done-ratio': return t('The percentage done of issues isn’t kept')
    case 'fields-guessed': return t('Custom fields were read without their definitions (an administrator’s key has them): their kinds are guessed from the values')
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
  const [source, setSource] = useState<Source>('xml-site-backup')
  // tokens by job, in this page's memory only: needed again to download attachments
  const [tokens, setTokens] = useState<Record<string, string>>({})
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
        {showCurrent || !isApi(source)
          ? t('Moving from another tracker? Download its XML site backup (a .zip with entities.xml and the attachments) and upload it here, or connect to its API. TaskCraft checks what it gets and shows it; nothing changes until you start the import. People, projects, issues with their numbers, comments, time logs, attachments, links, watchers, components, releases, sprints and the history the reports need come along. Nobody gets an e-mail.')
          : t('TaskCraft connects to {source} with a token that can read the projects, reads them and shows what they hold; nothing changes until you start the import. Issues with their numbers, comments, labels, assignees, releases, history and attached files come along. The token is only used while reading and importing and isn’t stored. Nobody gets an e-mail.', { source: sourceName(source) })}
      </p>
      {showCurrent ? (
        <CurrentImport job={current} token={tokens[current.id] ?? ''} onToken={(v) => setTokens((all) => ({ ...all, [current.id]: v }))} onChanged={refresh} onAnother={() => setAnother(true)} />
      ) : (
        <>
          <SourcePicker value={source} onChange={setSource} />
          {isApi(source) ? (
            <ConnectCard
              key={source}
              source={source}
              onConnected={(id, token) => {
                setTokens((all) => ({ ...all, [id]: token }))
                setAnother(false)
                refresh()
              }}
            />
          ) : (
            <UploadCard maxMb={jobs.data?.maxUploadMb ?? 256} onUploaded={() => { setAnother(false); refresh() }} />
          )}
        </>
      )}
      {list.length > (showCurrent ? 1 : 0) && <EarlierImports jobs={showCurrent ? list.slice(1) : list} />}
    </div>
  )
}

function SourcePicker({ value, onChange }: { value: Source; onChange: (s: Source) => void }) {
  return (
    <fieldset className="bi-sources">
      <legend>{t('Import from')}</legend>
      <div className="bi-source-list">
        {SOURCES.map((s) => (
          <label key={s.id} className={`bi-source ${value === s.id ? 'on' : ''}`}>
            <input type="radio" name="bi-source" value={s.id} checked={value === s.id} onChange={() => onChange(s.id)} />
            {isApi(s.id) ? <Plug size={14} /> : <FileArchive size={14} />}
            <span>{s.label()}</span>
          </label>
        ))}
      </div>
    </fieldset>
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

/** The address, a token and the projects to read from another tracker's API. */
function ConnectCard({ source, onConnected }: { source: ApiSource; onConnected: (id: string, token: string) => void }) {
  const f = apiFields(source)
  const [url, setUrl] = useState('')
  const [token, setToken] = useState('')
  const [owner, setOwner] = useState('')
  const [entry, setEntry] = useState('')
  const [chosen, setChosen] = useState<string[]>([])
  const [available, setAvailable] = useState<Available[] | null>(null)
  const [listing, setListing] = useState(false)
  const [sending, setSending] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const id = `bi-${source}`
  const add = (k: string) => {
    const v = k.trim().replace(/^\/+|\/+$/g, '')
    if (v && !chosen.includes(v)) setChosen((c) => [...c, v])
  }
  const toggle = (k: string) => setChosen((c) => (c.includes(k) ? c.filter((x) => x !== k) : [...c, k]))
  const connection = () => ({ source, baseUrl: url.trim(), token: token.trim() || null })
  const list = async () => {
    setListing(true)
    setError(null)
    try {
      const r = await api.post<{ projects: Available[] }>('/admin/backup-imports/browse', { ...connection(), owner: owner.trim() || null })
      setAvailable(r.projects)
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setListing(false)
    }
  }
  const submit = async (e: FormEvent) => {
    e.preventDefault()
    const all = [...chosen]
    if (entry.trim() && !all.includes(entry.trim())) all.push(entry.trim())
    if (!all.length) {
      setError(t('Choose at least one project to read.'))
      return
    }
    setSending(true)
    setError(null)
    try {
      const job = await api.post<Job>('/admin/backup-imports/connect', { ...connection(), projects: all })
      onConnected(job.id, token.trim())
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e))
    } finally {
      setSending(false)
    }
  }
  return (
    <form className="adm-card bi-card" onSubmit={submit}>
      <div className="adm-card-head">
        <h2><Plug size={16} /> {t('Connect to {source}', { source: sourceName(source) })}</h2>
        <p className="muted small">{t('Read access is enough. The token is used while reading and importing only; it isn’t stored.')}</p>
      </div>
      <div className="adm-card-body bi-connect">
        <div className="field">
          <label htmlFor={`${id}-url`}>{f.url}{!f.urlRequired && <span className="faint"> {t('(optional)')}</span>}</label>
          <input id={`${id}-url`} className="input" type="url" inputMode="url" placeholder={f.urlPlaceholder} value={url} required={f.urlRequired} onChange={(e) => setUrl(e.target.value)} aria-describedby={`${id}-url-hint`} />
          <span id={`${id}-url-hint`} className="faint small">{f.urlHint}</span>
        </div>
        <div className="field">
          <label htmlFor={`${id}-token`}>{f.token}</label>
          <input id={`${id}-token`} className="input" type="password" autoComplete="off" spellCheck={false} value={token} onChange={(e) => setToken(e.target.value)} aria-describedby={`${id}-token-hint`} />
          <span id={`${id}-token-hint`} className="faint small">{f.tokenHint}</span>
        </div>
        <fieldset className="bi-projects">
          <legend>{f.projects}</legend>
          {chosen.length > 0 && (
            <ul className="bi-chosen" aria-label={t('Chosen projects')}>
              {chosen.map((k) => (
                <li key={k}>
                  <span className="issue-key">{k}</span>
                  <button type="button" className="icon-btn" aria-label={t('Remove {name}', { name: k })} onClick={() => toggle(k)}><X size={13} /></button>
                </li>
              ))}
            </ul>
          )}
          <div className="bi-add">
            <input
              className="input"
              aria-label={t('Add a project by name')}
              placeholder={f.projectPlaceholder}
              value={entry}
              onChange={(e) => setEntry(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault()
                  add(entry)
                  setEntry('')
                }
              }}
            />
            <button type="button" className="btn" disabled={!entry.trim()} onClick={() => { add(entry); setEntry('') }}><Plus size={14} /> {t('Add')}</button>
          </div>
          <div className="bi-add">
            <input className="input" aria-label={f.owner} placeholder={f.owner} value={owner} onChange={(e) => setOwner(e.target.value)} />
            <button type="button" className="btn" disabled={listing} onClick={list}>{listing ? <Spinner size={14} /> : <ListChecks size={14} />} {f.list}</button>
          </div>
          {available && (
            available.length === 0 ? (
              <p className="faint small">{t('Nothing to choose from here.')}</p>
            ) : (
              <ul className="bi-available">
                {available.map((a) => (
                  <li key={a.key}>
                    <label className="bi-radio">
                      <input type="checkbox" checked={chosen.includes(a.key)} onChange={() => toggle(a.key)} />
                      <span>
                        <strong>{a.key}</strong>
                        <span className="faint small">
                          {[a.parent && t('in {parent}', { parent: a.parent }), a.issues != null && t('{count} open issues', { count: a.issues }), a.private && t('private'), a.description].filter(Boolean).join(' · ')}
                        </span>
                      </span>
                    </label>
                  </li>
                ))}
              </ul>
            )
          )}
        </fieldset>
        {error && <p className="error-text" role="alert">{error}</p>}
      </div>
      <div className="adm-card-foot">
        <button className="btn btn-primary" type="submit" disabled={sending}>{sending ? <Spinner size={14} /> : <Plug size={14} />} {t('Connect and check')}</button>
      </div>
    </form>
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

function phaseText(p: Progress | null, checking: boolean, api: boolean): string {
  if (!p || p.phase === 'reading') return api ? t('Reading…') : checking ? t('Reading and checking the backup…') : t('Reading the backup…')
  switch (p.phase) {
    case 'fetching':
      if (p.project && p.total) return t('Reading {project}: issue {done} of {total}', { project: p.project, done: p.done + 1, total: p.total })
      return p.project ? t('Reading {project}…', { project: p.project }) : t('Reading…')
    case 'waiting': return t('Waiting for the source’s rate limit until {time}…', { time: p.until ? new Intl.DateTimeFormat(locale(), { timeStyle: 'short' }).format(new Date(p.until)) : '' })
    case 'people': return checking ? t('Reading people…') : t('Matching people…')
    case 'issues': return t('Importing {project}: issue {done} of {total}', { project: p.project ?? '', done: p.done + 1, total: p.total })
    case 'links': return t('Connecting parents and links…')
    case 'saving': return t('Saving…')
    default: return t('Working…')
  }
}

function CurrentImport({ job, token, onToken, onChanged, onAnother }: { job: Job; token: string; onToken: (v: string) => void; onChanged: () => void; onAnother: () => void }) {
  const [discarding, setDiscarding] = useState(false)
  const fromApi = isApi(job.source)
  const head = (
    <div className="adm-card-head bi-head">
      <h2>{fromApi ? <Plug size={16} /> : <FileArchive size={16} />} {job.fileName}</h2>
      <p className="muted small">
        {fromApi ? t('{url} · connected {when}', { url: job.connection?.baseUrl ?? '', when: ago(job.createdAt) }) : `${bytes(job.size)} · ${t('uploaded {when}', { when: ago(job.createdAt) })}`}
      </p>
    </div>
  )
  if (job.status === 'checking' || job.status === 'running') {
    const p = job.progress
    const value = (p?.phase === 'issues' || p?.phase === 'fetching' || p?.phase === 'people') && p.total ? p.done / p.total : null
    return (
      <div className="adm-card bi-card" aria-busy="true">
        {head}
        <div className="adm-card-body" aria-live="polite">
          <Bar value={value} label={phaseText(p, job.status === 'checking', fromApi)} />
          <p className="faint small">
            {job.status === 'running' || fromApi ? t('This can take a while for big projects. You can leave this page; it goes on.') : t('This takes a moment.')}
          </p>
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
            <p className="bi-failed" role="alert">
              <CircleX size={16} /> {job.summary ? t('The import stopped; nothing was changed.') : fromApi ? t('The source couldn’t be read.') : t('The backup couldn’t be read.')}
            </p>
            {job.error && <pre className="bi-error">{job.error}</pre>}
          </div>
          <div className="adm-card-foot">
            <button className="btn" onClick={onAnother}><RotateCcw size={14} /> {fromApi ? t('Connect again') : t('Upload another backup')}</button>
          </div>
        </div>
      )}
      {job.status === 'done' && job.report && <ReportCard job={job} report={job.report} onAnother={onAnother} />}
      {job.summary && job.fileKept && (job.status === 'checked' || job.status === 'failed' || job.report?.dryRun) && (
        <Choices key={job.id} job={job} summary={job.summary} token={token} onToken={onToken} onStarted={onChanged} onDiscard={() => setDiscarding(true)} />
      )}
      {discarding && (
        <Confirm
          title={fromApi ? t('Discard what was read?') : t('Discard this backup?')}
          body={fromApi ? t('What TaskCraft read is deleted. Nothing was imported from it.') : t('The uploaded file is deleted. Nothing was imported from it.')}
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

function Choices({ job, summary, token, onToken, onStarted, onDiscard }: { job: Job; summary: Summary; token: string; onToken: (v: string) => void; onStarted: () => void; onDiscard: () => void }) {
  const toast = useToast()
  const projects = useProjects().data ?? []
  const open = projects.filter((p) => !p.archivedAt)
  const fromApi = isApi(job.source)
  const needsToken = fromApi && !!job.connection?.token
  const newKey = (p: ProjectSummary) => (p.validKey ? p.key : (p.suggestedKey ?? ''))
  const initial = (): Record<string, Choice> => {
    const out: Record<string, Choice> = {}
    for (const p of summary.projects) {
      const before = job.options?.projects?.[p.key]
      if (before) out[p.key] = before
      // a project with the same key here: into it (the source's earlier import, or the same team)
      else if (p.existing) out[p.key] = { action: 'into', key: p.existing.key }
      else out[p.key] = { action: 'new', key: newKey(p) }
    }
    return out
  }
  const [choices, setChoices] = useState<Record<string, Choice>>(initial)
  const [people, setPeople] = useState<People>(job.options?.people ?? 'invite')
  const [markup, setMarkup] = useState<Markup | null>(job.options?.markup ?? summary.markup ?? null)
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
  const missingToken = needsToken && !token.trim()
  const start = async (dryRun: boolean) => {
    setStarting(true)
    try {
      const body = Object.fromEntries(Object.entries(choices).map(([k, c]) => [k, c.action === 'skip' ? c : { ...c, key: c.key.trim().toUpperCase() }]))
      await api.post(`/admin/backup-imports/${job.id}/run`, { projects: body, people, dryRun, ...(fromApi ? { token: token.trim() || null, markup } : {}) })
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
        <h2>{fromApi ? t('What {source} holds', { source: sourceName(job.source) }) : t('What the backup holds')}</h2>
        <p className="muted small">
          {t('{count} people are named in it; {matched} of them have an account here already (same e-mail address or name).', { count: summary.people.total, matched: summary.people.matched })}
          {' '}{t('{count} attachment files.', { count: summary.files })}
        </p>
      </div>
      <div className="adm-card-body">
        <div className="adm-table-wrap">
          <table className="list adm-table bi-table">
            <caption className="sr-only">{fromApi ? t('Projects read') : t('Projects in the backup')}</caption>
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
                      <div className="bi-project"><span className="issue-key">{p.key}</span> {p.name !== p.key && <span>{p.name}</span>}</div>
                      {p.parent && <div className="faint small">{t('a subproject of {parent}', { parent: p.parent })}</div>}
                      {p.formerKeys.length > 0 && <div className="faint small">{t('formerly {keys}', { keys: p.formerKeys.join(', ') })}</div>}
                      {p.sprints > 0 && <div className="faint small">{t('{count} sprints', { count: p.sprints })}</div>}
                      {(p.confidential ?? 0) > 0 && <div className="faint small">{t('{count} confidential issues', { count: p.confidential ?? 0 })}</div>}
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
                          const k = newKey(p)
                          if (a === 'skip') set(p.key, { action: 'skip' })
                          else if (a === 'new') set(p.key, { action: 'new', key: taken.has(k) ? '' : k })
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
              <span className="faint small">{t('Accounts waiting for an invitation, which you send from Users when you’re ready. People who are inactive in the source get deactivated accounts.')}</span>
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
        {summary.markup && (
          <div className="field bi-markup">
            <label htmlFor={`bi-markup-${job.id}`}>{t('Texts are written in')}</label>
            <select id={`bi-markup-${job.id}`} className="select" value={markup ?? summary.markup} onChange={(e) => setMarkup(e.target.value as Markup)} aria-describedby={`bi-markup-hint-${job.id}`}>
              <option value="textile">Textile</option>
              <option value="markdown">Markdown</option>
            </select>
            <span id={`bi-markup-hint-${job.id}`} className="faint small">{t('Found from the texts; change it if the server is set up otherwise.')}</span>
          </div>
        )}
        {needsToken && (
          <div className="field bi-markup">
            <label htmlFor={`bi-token-${job.id}`}>{t('Token')}</label>
            <input id={`bi-token-${job.id}`} className="input" type="password" autoComplete="off" spellCheck={false} value={token} onChange={(e) => onToken(e.target.value)} aria-describedby={`bi-token-hint-${job.id}`} />
            <span id={`bi-token-hint-${job.id}`} className="faint small">{t('The import downloads the attachments with it. It isn’t stored: after a reload, enter it again.')}</span>
          </div>
        )}
        <Warnings list={summary.warnings} />
        {problems.map((m, i) => <p key={i} className="error-text">{m}</p>)}
      </div>
      <div className="adm-card-foot">
        <span className="adm-foot-msg faint small">{t('Kept until {when}, then deleted.', { when: dateTime(job.expiresAt) })}</span>
        <button className="btn" onClick={onDiscard} disabled={starting}>{t('Discard')}</button>
        <button className="btn" onClick={() => start(true)} disabled={starting || problems.length > 0}>{t('Test the import')}</button>
        <button className="btn btn-primary" onClick={() => start(false)} disabled={starting || problems.length > 0 || missingToken} title={missingToken ? t('Enter the token first') : undefined}>
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
  ['fields', () => t('Custom fields')],
  ['confidential', () => t('Confidential issues')],
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
              {r.source !== r.key && <p className="faint small">{isApi(job.source) ? t('{old} in {source}', { old: r.source, source: sourceName(job.source) }) : t('{old} in the backup', { old: r.source })}</p>}
              {r.parent && <p className="faint small">{t('a subproject of {parent}', { parent: r.parent })}</p>}
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
        {!report.dryRun && <button className="btn" onClick={onAnother}><Upload size={14} /> {t('Import something else')}</button>}
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
      <h3>{t('Earlier imports')}</h3>
      <ul>
        {jobs.map((j) => (
          <li key={j.id}>
            {isApi(j.source) ? <Plug size={14} /> : <FileArchive size={14} />} <span>{j.fileName}</span>
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
