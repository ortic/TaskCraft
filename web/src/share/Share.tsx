// Shared pages (/share/:token): what a project admin shares by link with people without an
// account — a list of issues (a status page) or the roadmap — read-only, in the visitor's browser
// language. A password, when the link has one, is asked first; the key it unlocks stays in this
// browser tab.
import { locale, t } from '../i18n'
import { useEffect, useMemo, useRef, useState, type FormEvent } from 'react'
import { Link, Route, Routes, useLocation, useParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, Lock, Search } from 'lucide-react'
import { ApiError, ago, shortDate, type Category } from '../api'
import { applyAccent } from '../look'
import { Lozenge, Spinner } from '../ui'
import RichDoc, { type DocNode } from '../portal/RichDoc'
import '../portal/portal.css'
import './share.css'

interface Info {
  title: string
  kind: 'issues' | 'roadmap'
  locked: boolean
  project: { name: string; key: string; color: string | null }
  workspace: { name: string; accentColor?: string | null }
  people: boolean
  comments: boolean
  expiresAt: string | null
}
interface Shared {
  key: string
  title: string
  type: { name: string; level: number }
  status: { name: string; category: Category; color: string | null }
  priority: string
  assignee: { name: string } | null
  startDate: string | null
  dueDate: string | null
  updatedAt: string
  resolvedAt: string | null
  createdAt: string
  labels: string[]
  versions: string[]
  childCount: number
  childDone: number
}
interface Detail extends Shared {
  description: DocNode | null
  children: Shared[]
  comments: { body: DocNode; author: string | null; createdAt: string }[]
}
interface Release {
  name: string
  released: boolean
  startDate: string | null
  releaseDate: string | null
}

const keyStore = (token: string) => `tc-share-${token}`

function storedKey(token: string): string {
  try {
    return sessionStorage.getItem(keyStore(token)) ?? ''
  } catch {
    return ''
  }
}

/** Requests to the share with its key (when it has a password). */
async function shareGet<T>(token: string, path: string): Promise<T> {
  const key = storedKey(token)
  const res = await fetch(`/api/share/${encodeURIComponent(token)}${path}`, { headers: key ? { 'x-share-key': key } : {} })
  const data = await res.json().catch(() => null)
  if (!res.ok) throw new ApiError(res.status, data?.error ?? 'error', data?.message ?? res.statusText)
  return data as T
}

export default function SharedApp() {
  return (
    <Routes>
      <Route path="/share/:token" element={<SharePage />} />
      <Route path="/share/:token/:key" element={<SharePage />} />
    </Routes>
  )
}

function SharePage() {
  const { token = '', key } = useParams()
  const loc = useLocation()
  const main = useRef<HTMLElement>(null)
  const first = useRef(true)
  const info = useQuery({ queryKey: ['share', token], queryFn: () => shareGet<Info>(token, ''), retry: false })
  const accent = info.data?.workspace.accentColor
  useEffect(() => applyAccent(accent), [accent])
  useEffect(() => {
    if (info.data) document.title = `${info.data.title} · ${info.data.workspace.name}`
  }, [info.data])
  useEffect(() => {
    if (first.current) {
      first.current = false
      return
    }
    main.current?.focus()
  }, [loc.pathname])
  const d = info.data
  return (
    <div className="portal share">
      <a href="#share-main" className="portal-skip">{t('Skip to content')}</a>
      <header className="portal-header">
        <div className="portal-bar share-bar">
          <Link to={`/share/${token}`} className="portal-brand">
            {d ? (
              <>
                <span>{d.workspace.name}</span>
                <span className="portal-brand-sub">{d.project.name}</span>
              </>
            ) : (
              <span>{t('Shared page')}</span>
            )}
          </Link>
          <span className="share-badge">{t('Read only')}</span>
        </div>
      </header>
      <main id="share-main" ref={main} tabIndex={-1} className="portal-main share-main">
        {info.isLoading ? (
          <div className="portal-busy"><Spinner /></div>
        ) : info.error || !d ? (
          <Gone />
        ) : d.locked ? (
          <Unlock token={token} title={d.title} />
        ) : key ? (
          <IssuePage token={token} issueKey={key} info={d} />
        ) : d.kind === 'roadmap' ? (
          <Roadmap token={token} info={d} />
        ) : (
          <Issues token={token} info={d} />
        )}
      </main>
      <footer className="share-foot">
        {d && !d.locked && d.expiresAt && <span>{t('This link works until {date}.', { date: shortDate(d.expiresAt) })} </span>}
        <span>{t('Shared with TaskCraft.')}</span>
      </footer>
    </div>
  )
}

function Gone() {
  return (
    <div className="portal-card">
      <h1>{t('This link doesn’t work')}</h1>
      <p>{t('It may have ended or been taken back. Ask the person who sent it for a new one.')}</p>
    </div>
  )
}

function Unlock({ token, title }: { token: string; title: string }) {
  const qc = useQueryClient()
  const [password, setPassword] = useState('')
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)
  const submit = async (e: FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    try {
      const res = await fetch(`/api/share/${encodeURIComponent(token)}/unlock`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ password }),
      })
      const data = await res.json().catch(() => null)
      if (!res.ok) throw new Error(res.status === 400 ? t('That’s not the password.') : (data?.message ?? res.statusText))
      try {
        sessionStorage.setItem(keyStore(token), data.key ?? '')
      } catch { /* asked again on the next page */ }
      await qc.invalidateQueries({ queryKey: ['share', token] })
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <form className="portal-card share-unlock" onSubmit={submit}>
      <h1><Lock size={20} aria-hidden /> {title}</h1>
      <p>{t('This page is protected with a password.')}</p>
      <div className="field">
        <label htmlFor="share-password">{t('Password')}</label>
        <input id="share-password" type="password" className="input" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="current-password" required autoFocus />
      </div>
      {err && <p className="portal-error" role="alert">{err}</p>}
      <div className="portal-actions">
        <button className="btn btn-primary" disabled={busy || !password}>{t('Open')}</button>
      </div>
    </form>
  )
}

const OPEN: Category[] = ['todo', 'in_progress']

function Issues({ token, info }: { token: string; info: Info }) {
  const [q, setQ] = useState('')
  const list = useQuery({ queryKey: ['share', token, 'issues'], queryFn: () => shareGet<{ issues: Shared[]; more: boolean }>(token, '/issues') })
  const all = list.data?.issues ?? []
  const needle = q.trim().toLowerCase()
  const shown = needle ? all.filter((i) => i.title.toLowerCase().includes(needle) || i.key.toLowerCase().includes(needle)) : all
  const open = shown.filter((i) => OPEN.includes(i.status.category))
  const done = shown.filter((i) => !OPEN.includes(i.status.category))
  return (
    <>
      <h1>{info.title}</h1>
      <p className="portal-lead">
        {list.data ? t('{open} open, {done} done', { open: all.filter((i) => OPEN.includes(i.status.category)).length, done: all.filter((i) => !OPEN.includes(i.status.category)).length }) : ' '}
      </p>
      {all.length > 8 && (
        <div className="share-filter">
          <Search size={15} aria-hidden />
          <input className="input" type="search" placeholder={t('Filter by title or key')} aria-label={t('Filter by title or key')} value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
      )}
      {list.isLoading ? (
        <div className="portal-busy"><Spinner /></div>
      ) : list.error ? (
        <p className="portal-error">{(list.error as Error).message}</p>
      ) : (
        <>
          <IssueTable token={token} title={t('Open')} issues={open} people={info.people} />
          <IssueTable token={token} title={t('Done')} issues={done} people={info.people} />
          {shown.length === 0 && <p className="portal-note">{needle ? t('Nothing matches “{q}”.', { q }) : t('Nothing here yet.')}</p>}
          {list.data?.more && <p className="portal-faint">{t('Only the first 500 are shown.')}</p>}
        </>
      )}
    </>
  )
}

function IssueTable({ token, title, issues, people }: { token: string; title: string; issues: Shared[]; people: boolean }) {
  if (issues.length === 0) return null
  return (
    <section className="portal-section">
      <h2>{title} <span className="badge">{issues.length}</span></h2>
      <ul className="share-list">
        {issues.map((i) => (
          <li key={i.key}>
            <Link to={`/share/${token}/${i.key}`} className="share-row">
              <span className="share-key">{i.key}</span>
              <span className="share-title">{i.title}</span>
              <span className="share-meta">
                <Lozenge name={i.status.name} category={i.status.category} color={i.status.color} />
                {people && i.assignee && <span className="portal-faint">{i.assignee.name}</span>}
                <span className="portal-faint">{i.resolvedAt ? t('done {when}', { when: ago(i.resolvedAt) }) : t('updated {when}', { when: ago(i.updatedAt) })}</span>
              </span>
            </Link>
          </li>
        ))}
      </ul>
    </section>
  )
}

/** Months between two days, for the timeline's header. */
function months(from: Date, to: Date): Date[] {
  const out: Date[] = []
  const d = new Date(from.getFullYear(), from.getMonth(), 1)
  while (d <= to && out.length < 37) {
    out.push(new Date(d))
    d.setMonth(d.getMonth() + 1)
  }
  return out
}

const day = (s: string) => new Date(s + 'T00:00:00')

function Roadmap({ token, info }: { token: string; info: Info }) {
  const r = useQuery({ queryKey: ['share', token, 'roadmap'], queryFn: () => shareGet<{ epics: Shared[]; releases: Release[] }>(token, '/roadmap') })
  const epics = useMemo(() => r.data?.epics ?? [], [r.data])
  const releases = useMemo(() => (r.data?.releases ?? []).filter((x) => x.releaseDate), [r.data])
  const range = useMemo(() => {
    const days = [
      ...epics.flatMap((e) => [e.startDate, e.dueDate]),
      ...releases.flatMap((x) => [x.startDate, x.releaseDate]),
    ].filter((x): x is string => !!x).map(day)
    if (days.length === 0) return null
    const from = new Date(Math.min(...days.map((d) => d.getTime())))
    const to = new Date(Math.max(...days.map((d) => d.getTime())))
    const start = new Date(from.getFullYear(), from.getMonth(), 1)
    const end = new Date(to.getFullYear(), to.getMonth() + 1, 1)
    return { start, end, span: end.getTime() - start.getTime() }
  }, [epics, releases])
  const pos = (d: Date) => (range ? ((d.getTime() - range.start.getTime()) / range.span) * 100 : 0)
  const fmtMonth = new Intl.DateTimeFormat(locale(), { month: 'short', year: 'numeric' })
  const today = new Date()
  return (
    <>
      <h1>{info.title}</h1>
      {r.isLoading ? (
        <div className="portal-busy"><Spinner /></div>
      ) : r.error ? (
        <p className="portal-error">{(r.error as Error).message}</p>
      ) : epics.length === 0 && releases.length === 0 ? (
        <p className="portal-note">{t('Nothing planned yet.')}</p>
      ) : (
        <>
          {range && (
            <div className="share-road" role="presentation">
              <div className="share-road-months" aria-hidden>
                {months(range.start, new Date(range.end.getTime() - 1)).map((m) => (
                  <span key={m.toISOString()} style={{ left: `${pos(m)}%` }}>{fmtMonth.format(m)}</span>
                ))}
              </div>
              {today >= range.start && today < range.end && <div className="share-road-today" style={{ left: `${pos(today)}%` }} aria-hidden />}
              {epics.map((e) => {
                const s = e.startDate ?? e.dueDate
                const f = e.dueDate ?? e.startDate
                const progress = e.childCount ? Math.round((e.childDone / e.childCount) * 100) : e.status.category === 'done' ? 100 : 0
                return (
                  <div key={e.key} className="share-road-row">
                    {s && f ? (
                      <Link
                        to={`/share/${token}/${e.key}`}
                        className={`share-road-bar share-road-${e.status.category}`}
                        style={{ left: `${pos(day(s))}%`, width: `max(${pos(new Date(day(f).getTime() + 86400000)) - pos(day(s))}%, 6px)` }}
                        aria-label={t('{title}: {from} to {to}, {progress}% done', { title: e.title, from: shortDate(s), to: shortDate(f), progress })}
                      >
                        <span className="share-road-fill" style={{ width: `${progress}%` }} aria-hidden />
                        <span className="share-road-label" aria-hidden>{e.title}</span>
                      </Link>
                    ) : null}
                  </div>
                )
              })}
              {releases.map((x) => (
                <div key={x.name} className={`share-road-release ${x.released ? 'released' : ''}`} style={{ left: `${pos(day(x.releaseDate!))}%` }} aria-hidden>
                  <span>{x.name}</span>
                </div>
              ))}
            </div>
          )}
          <section className="portal-section">
            <h2>{t('Epics')}</h2>
            <ul className="share-list">
              {epics.map((e) => (
                <li key={e.key}>
                  <Link to={`/share/${token}/${e.key}`} className="share-row">
                    <span className="share-key">{e.key}</span>
                    <span className="share-title">{e.title}</span>
                    <span className="share-meta">
                      <Lozenge name={e.status.name} category={e.status.category} color={e.status.color} />
                      <span className="portal-faint">
                        {e.startDate || e.dueDate ? `${shortDate(e.startDate ?? e.dueDate)} – ${shortDate(e.dueDate ?? e.startDate)}` : t('No dates')}
                        {e.childCount > 0 && ` · ${t('{done} of {total} done', { done: e.childDone, total: e.childCount })}`}
                      </span>
                    </span>
                  </Link>
                </li>
              ))}
            </ul>
          </section>
          {releases.length > 0 && (
            <section className="portal-section">
              <h2>{t('Releases')}</h2>
              <ul className="share-list">
                {releases.map((x) => (
                  <li key={x.name} className="share-row share-row-static">
                    <span className="share-title">{x.name}</span>
                    <span className="share-meta">
                      <span className={`lozenge lozenge-${x.released ? 'done' : 'todo'}`}>{x.released ? t('Released') : t('Planned')}</span>
                      <span className="portal-faint">{shortDate(x.releaseDate)}</span>
                    </span>
                  </li>
                ))}
              </ul>
            </section>
          )}
        </>
      )}
    </>
  )
}

function IssuePage({ token, issueKey, info }: { token: string; issueKey: string; info: Info }) {
  const q = useQuery({ queryKey: ['share', token, 'issue', issueKey], queryFn: () => shareGet<Detail>(token, `/issues/${encodeURIComponent(issueKey)}`), retry: false })
  const key = storedKey(token)
  // images come through the share (with its key when it has a password)
  const images = (src: string) => (src.startsWith(`/api/share/${token}/files/`) ? (key ? `${src}?key=${encodeURIComponent(key)}` : src) : null)
  const i = q.data
  return (
    <>
      <p className="share-back">
        <Link to={`/share/${token}`}><ArrowLeft size={15} aria-hidden /> {info.title}</Link>
      </p>
      {q.isLoading ? (
        <div className="portal-busy"><Spinner /></div>
      ) : q.error || !i ? (
        <p className="portal-note">{t('This issue isn’t on the shared page.')}</p>
      ) : (
        <article>
          <p className="portal-sub">{i.key} · {i.type.name}</p>
          <h1>{i.title}</h1>
          <div className="share-facts">
            <Lozenge name={i.status.name} category={i.status.category} color={i.status.color} />
            {info.people && i.assignee && <span>{t('Assignee: {name}', { name: i.assignee.name })}</span>}
            {(i.startDate || i.dueDate) && <span>{i.startDate && i.dueDate ? `${shortDate(i.startDate)} – ${shortDate(i.dueDate)}` : t('Due {date}', { date: shortDate(i.dueDate ?? i.startDate) })}</span>}
            <span className="portal-faint">{t('updated {when}', { when: ago(i.updatedAt) })}</span>
          </div>
          {i.description && (
            <div className="portal-doc share-doc">
              <RichDoc doc={i.description} images={images} />
            </div>
          )}
          {i.children.length > 0 && (
            <section className="portal-section">
              <h2>{t('{done} of {total} done', { done: i.children.filter((c) => c.status.category === 'done').length, total: i.children.length })}</h2>
              <ul className="share-list">
                {i.children.map((c) => (
                  <li key={c.key} className="share-row share-row-static">
                    <span className="share-key">{c.key}</span>
                    <span className="share-title">{c.title}</span>
                    <span className="share-meta"><Lozenge name={c.status.name} category={c.status.category} color={c.status.color} /></span>
                  </li>
                ))}
              </ul>
            </section>
          )}
          {info.comments && i.comments.length > 0 && (
            <section className="portal-section">
              <h2>{t('Updates')}</h2>
              <ol className="share-comments">
                {i.comments.map((c, n) => (
                  <li key={n}>
                    <p className="portal-faint">{c.author ? t('{name}, {when}', { name: c.author, when: ago(c.createdAt) }) : ago(c.createdAt)}</p>
                    <div className="portal-doc"><RichDoc doc={c.body} images={images} /></div>
                  </li>
                ))}
              </ol>
            </section>
          )}
        </article>
      )}
    </>
  )
}
