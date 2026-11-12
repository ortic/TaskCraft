// The service desk portal (/portal): people without an account follow and answer their requests
// (signed in by a link e-mailed to them) and use the projects' public request forms. A small app
// of its own: nothing of the workspace beyond their requests, in the visitor's browser language.
import { t } from '../i18n'
import { useEffect, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { Link, Route, Routes, useLocation, useNavigate, useParams, useSearchParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowLeft, CircleCheck, FileText, Paperclip } from 'lucide-react'
import { api, ApiError, ago, dateTime, shortDate } from '../api'
import { applyAccent } from '../look'
import { Spinner } from '../ui'
import RichDoc, { docText, type DocNode } from './RichDoc'
import './portal.css'

type State = 'open' | 'in_progress' | 'resolved'
type Show = 'off' | 'optional' | 'required'

interface Info {
  workspace: { name: string; accentColor?: string | null }
  /** the signed-in requester's address */
  email: string | null
  /** signing in works (e-mail is set up) */
  signIn: boolean
  forms: { path: string; title: string; project: string }[]
}
interface Listed {
  key: string
  title: string
  project: string
  state: State
  createdAt: string
  updatedAt: string
  resolvedAt: string | null
}
interface Message {
  id: string
  mine: boolean
  author: string | null
  body: DocNode
  createdAt: string
  edited: boolean
}
interface FileInfo {
  id: string
  name: string
  mime: string
  size: number
  createdAt: string
  url: string
}
interface Detail extends Listed {
  description: DocNode | null
  messages: Message[]
  files: FileInfo[]
  canReply: boolean
  canResolve: boolean
  limits: { files: number; mb: number; text: number }
}
interface Field {
  id: string
  name: string
  kind: 'text' | 'number' | 'select' | 'multiselect' | 'date' | 'checkbox' | 'url'
  description: string | null
  required: boolean
  options: { id: string; label: string }[]
}
interface FormDef {
  path: string
  project: string
  title: string
  intro: string
  success: string
  description: Show
  name: Show
  types: { id: string; name: string }[]
  priority: boolean
  fields: Field[]
  stamp: string
  minSeconds: number
}

const useInfo = () => useQuery({ queryKey: ['portal'], queryFn: () => api.get<Info>('/portal') })

const STATES: Record<State, () => string> = {
  open: () => t('Open [[request state]]'),
  in_progress: () => t('In progress [[request state]]'),
  resolved: () => t('Resolved [[request state]]'),
}

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e))
const isStatus = (e: unknown, status: number) => e instanceof ApiError && e.status === status
const KEY = /^[A-Za-z0-9_]+-\d+$/

/** Sets the page title: "<page> · <workspace>". */
function useTitle(title: string) {
  const name = useInfo().data?.workspace.name
  useEffect(() => {
    document.title = name ? `${title} · ${name}` : title
  }, [title, name])
}

/** Paragraphs of plain text (blank lines between them). */
function Paragraphs({ text, className }: { text: string; className?: string }) {
  return (
    <>
      {text
        .split(/\n\s*\n/)
        .map((p) => p.trim())
        .filter(Boolean)
        .map((p, i) => <p key={i} className={className}>{p}</p>)}
    </>
  )
}

function fileSize(n: number): string {
  const fmt = (v: number) => new Intl.NumberFormat(undefined, { maximumFractionDigits: v < 10 ? 1 : 0 }).format(v)
  if (n < 1024 * 1024) return `${fmt(Math.max(n, 1) / 1024)} KB`
  return `${fmt(n / 1024 / 1024)} MB`
}

function Busy() {
  return (
    <div className="portal-busy">
      <Spinner />
    </div>
  )
}

function Problem({ error }: { error: unknown }) {
  return <p className="portal-error" role="alert">{errText(error)}</p>
}

/** Takes the focus when it appears (so screen readers read what changed). */
function useFocusOnMount<T extends HTMLElement>() {
  const ref = useRef<T>(null)
  useEffect(() => ref.current?.focus(), [])
  return ref
}

function StateBadge({ state }: { state: State }) {
  return <span className={`portal-state portal-state-${state}`}>{STATES[state]()}</span>
}

export default function Portal() {
  const info = useInfo()
  const loc = useLocation()
  const main = useRef<HTMLElement>(null)
  const first = useRef(true)
  const accent = info.data?.workspace.accentColor
  useEffect(() => applyAccent(accent), [accent])
  // a new page: the focus moves to its content, as after a page load
  useEffect(() => {
    if (first.current) {
      first.current = false
      return
    }
    main.current?.focus()
  }, [loc.pathname])
  const name = info.data?.workspace.name
  return (
    <div className="portal">
      <a href="#portal-main" className="portal-skip">{t('Skip to content')}</a>
      <header className="portal-header">
        <div className="portal-bar">
          <Link to="/portal" className="portal-brand">
            {name && <span>{name}</span>}
            <span className="portal-brand-sub">{t('Support')}</span>
          </Link>
          {info.data?.email && <SignedIn email={info.data.email} />}
        </div>
      </header>
      <main id="portal-main" ref={main} tabIndex={-1} className="portal-main">
        {info.isLoading ? (
          <Busy />
        ) : info.error || !info.data ? (
          <Problem error={info.error} />
        ) : (
          <Routes>
            <Route path="/portal" element={<Home info={info.data} />} />
            <Route path="/portal/sign-in" element={<UseLink info={info.data} />} />
            <Route path="/portal/requests/:key" element={<RequestPage info={info.data} />} />
            <Route path="/portal/forms/:path" element={<FormPage info={info.data} />} />
            <Route path="*" element={<NotFound />} />
          </Routes>
        )}
      </main>
    </div>
  )
}

function SignedIn({ email }: { email: string }) {
  const qc = useQueryClient()
  const nav = useNavigate()
  return (
    <div className="portal-who">
      <span className="portal-who-email" title={email}>{email}</span>
      <button
        type="button"
        className="btn btn-sm"
        onClick={async () => {
          try {
            await api.del('/portal/session')
          } finally {
            qc.removeQueries({ queryKey: ['portal-requests'] })
            qc.removeQueries({ queryKey: ['portal-request'] })
            await qc.invalidateQueries({ queryKey: ['portal'] })
            nav('/portal')
          }
        }}
      >
        {t('Sign out')}
      </button>
    </div>
  )
}

function NotFound() {
  useTitle(t('Not found'))
  return (
    <>
      <h1>{t('Not found')}</h1>
      <p><Link to="/portal">{t('Go to the start page')}</Link></p>
    </>
  )
}

// ---- start page --------------------------------------------------------------------------------

function Home({ info }: { info: Info }) {
  useTitle(info.email ? t('Your requests') : t('Follow your requests'))
  if (info.email)
    return (
      <>
        <Requests />
        <Forms forms={info.forms} heading={t('Ask for something new')} />
      </>
    )
  return (
    <>
      <h1>{t('Follow your requests')}</h1>
      <p className="portal-lead">{t('Sign in with your e-mail address to see your requests, their status and our answers, and to write to us.')}</p>
      <SignInForm info={info} />
      <Forms forms={info.forms} heading={t('Ask for help')} />
    </>
  )
}

function Requests() {
  const q = useQuery({ queryKey: ['portal-requests'], queryFn: () => api.get<{ requests: Listed[] }>('/portal/requests') })
  return (
    <section aria-labelledby="portal-requests-h">
      <h1 id="portal-requests-h">{t('Your requests')}</h1>
      {q.isLoading ? (
        <Busy />
      ) : q.error || !q.data ? (
        <Problem error={q.error} />
      ) : q.data.requests.length === 0 ? (
        <p className="portal-lead">{t('There are no requests for your address yet.')}</p>
      ) : (
        <ul className="portal-list">
          {q.data.requests.map((r) => (
            <li key={r.key}>
              <Link to={`/portal/requests/${r.key}`} className="portal-item">
                <span className="portal-item-title">{r.title}</span>
                <span className="portal-item-meta">
                  <StateBadge state={r.state} />
                  <span>{r.key}</span>
                  <span>{r.project}</span>
                  <span>{t('updated {when}', { when: ago(r.updatedAt) })}</span>
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </section>
  )
}

function Forms({ forms, heading }: { forms: Info['forms']; heading: string }) {
  if (!forms.length) return null
  return (
    <section className="portal-section" aria-labelledby="portal-forms-h">
      <h2 id="portal-forms-h">{heading}</h2>
      <ul className="portal-list">
        {forms.map((f) => (
          <li key={f.path}>
            <Link to={`/portal/forms/${f.path}`} className="portal-item">
              <span className="portal-item-title"><FileText size={16} aria-hidden /> {f.title || t('Submit a request')}</span>
              <span className="portal-item-meta"><span>{f.project}</span></span>
            </Link>
          </li>
        ))}
      </ul>
    </section>
  )
}

// ---- signing in --------------------------------------------------------------------------------

function SignInForm({ info, request }: { info: Info; request?: string }) {
  const [email, setEmail] = useState('')
  const [sent, setSent] = useState<string | null>(null)
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)
  if (!info.signIn) return <p className="portal-note">{t('Signing in isn’t available here: e-mail isn’t set up.')}</p>
  if (sent) return <LinkSent email={sent} onAgain={() => setSent(null)} />
  return (
    <form
      className="portal-card"
      onSubmit={async (e) => {
        e.preventDefault()
        setBusy(true)
        setErr('')
        try {
          await api.post('/portal/sign-in', { email: email.trim(), request: request && KEY.test(request) ? request : undefined })
          setSent(email.trim())
        } catch (e) {
          setErr(errText(e))
        } finally {
          setBusy(false)
        }
      }}
    >
      <div className="field">
        <label htmlFor="portal-email">{t('Your e-mail address')}</label>
        <input id="portal-email" className="input" type="email" required autoComplete="email" value={email} onChange={(e) => setEmail(e.target.value)} />
      </div>
      {err && <p className="portal-error" role="alert">{err}</p>}
      <button className="btn btn-primary" disabled={busy}>{busy ? t('Sending…') : t('E-mail me a sign-in link')}</button>
    </form>
  )
}

function LinkSent({ email, onAgain }: { email: string; onAgain: () => void }) {
  const ref = useFocusOnMount<HTMLDivElement>()
  return (
    <div className="portal-card portal-done" ref={ref} tabIndex={-1}>
      <p><strong>{t('Check your e-mail.')}</strong></p>
      <p>{t('If there are requests for {email}, we’ve sent a link there to sign in. It works once, within 30 minutes.', { email })}</p>
      <button type="button" className="btn" onClick={onAgain}>{t('Use another address')}</button>
    </div>
  )
}

/** The page a sign-in link opens. It signs in only when asked: mail scanners open the links in
 *  e-mails too, and a link works once. */
function UseLink({ info }: { info: Info }) {
  const [params] = useSearchParams()
  const token = params.get('token') ?? ''
  const request = params.get('request') ?? undefined
  const qc = useQueryClient()
  const nav = useNavigate()
  const [busy, setBusy] = useState(false)
  const [failed, setFailed] = useState(false)
  useTitle(t('Sign in'))
  if (!token || failed)
    return (
      <>
        <h1>{t('Sign in')}</h1>
        <p className="portal-lead" role={failed ? 'alert' : undefined}>
          {failed ? t('This sign-in link has expired or was already used. Ask for a new one:') : t('Ask for a sign-in link:')}
        </p>
        <SignInForm info={info} request={request} />
      </>
    )
  return (
    <>
      <h1>{t('Sign in')}</h1>
      <div className="portal-card">
        <p>{t('Continue to see your requests and our answers.')}</p>
        <button
          type="button"
          className="btn btn-primary"
          disabled={busy}
          onClick={async () => {
            setBusy(true)
            try {
              await api.post('/portal/session', { token })
              qc.removeQueries({ queryKey: ['portal-requests'] })
              qc.removeQueries({ queryKey: ['portal-request'] })
              await qc.invalidateQueries({ queryKey: ['portal'] })
              nav(request && KEY.test(request) ? `/portal/requests/${request}` : '/portal', { replace: true })
            } catch {
              setFailed(true)
              setBusy(false)
            }
          }}
        >
          {busy ? t('Signing in…') : t('Continue')}
        </button>
      </div>
    </>
  )
}

// ---- a request ---------------------------------------------------------------------------------

function BackLink() {
  return (
    <p className="portal-back">
      <Link to="/portal"><ArrowLeft size={14} aria-hidden /> {t('Your requests')}</Link>
    </p>
  )
}

function RequestPage({ info }: { info: Info }) {
  const { key = '' } = useParams()
  const signedIn = !!info.email
  const q = useQuery({
    queryKey: ['portal-request', key.toUpperCase()],
    queryFn: () => api.get<Detail>(`/portal/requests/${encodeURIComponent(key)}`),
    enabled: signedIn,
    retry: false,
  })
  const [news, setNews] = useState('')
  useTitle(q.data ? `${q.data.title} (${q.data.key})` : t('Request {key}', { key }))
  if (!signedIn || isStatus(q.error, 401))
    return (
      <>
        <h1>{t('Sign in to see request {key}', { key })}</h1>
        <p className="portal-lead">{t('We’ll e-mail you a link to sign in. Use the address you wrote to us from.')}</p>
        <SignInForm info={info} request={key} />
      </>
    )
  if (q.isLoading) return <Busy />
  if (isStatus(q.error, 404))
    return (
      <>
        <BackLink />
        <h1>{t('Request not found')}</h1>
        <p className="portal-lead">{t('There’s no request {key} for {email}.', { key, email: info.email })}</p>
      </>
    )
  if (q.error || !q.data) return <Problem error={q.error} />
  const r = q.data
  return (
    <article className="portal-request" aria-labelledby="portal-request-h">
      <BackLink />
      <h1 id="portal-request-h">{r.title}</h1>
      <p className="portal-meta">
        <StateBadge state={r.state} />
        <span>{r.key}</span>
        <span>{r.project}</span>
        <span title={dateTime(r.createdAt)}>{t('opened {date}', { date: shortDate(r.createdAt) })}</span>
        <span title={dateTime(r.updatedAt)}>{t('updated {when}', { when: ago(r.updatedAt) })}</span>
      </p>
      <div className="portal-news" role="status">{news}</div>

      {docText(r.description).trim() && (
        <section className="portal-section" aria-labelledby="portal-desc-h">
          <h2 id="portal-desc-h">{t('Your request')}</h2>
          <div className="portal-doc"><RichDoc doc={r.description} /></div>
        </section>
      )}

      {r.files.length > 0 && (
        <section className="portal-section" aria-labelledby="portal-files-h">
          <h2 id="portal-files-h">{t('Your files')}</h2>
          <ul className="portal-files">
            {r.files.map((f) => (
              <li key={f.id}>
                <Paperclip size={14} aria-hidden />
                <a href={f.url}>{f.name}</a>
                <span className="portal-faint">{fileSize(f.size)}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="portal-section" aria-labelledby="portal-conv-h">
        <h2 id="portal-conv-h">{t('Conversation')}</h2>
        {r.messages.length === 0 ? (
          <p className="portal-faint">{t('No answers yet: we’ll write here and by e-mail.')}</p>
        ) : (
          <ol className="portal-messages">
            {r.messages.map((m) => (
              <li key={m.id} className={`portal-message${m.mine ? ' mine' : ''}`}>
                <div className="portal-message-head">
                  <strong>{m.mine ? t('You') : (m.author ?? t('Support team'))}</strong>
                  <time dateTime={m.createdAt} title={dateTime(m.createdAt)}>{ago(m.createdAt)}</time>
                  {m.edited && <span className="portal-faint">({t('edited')})</span>}
                </div>
                <div className="portal-doc"><RichDoc doc={m.body} /></div>
              </li>
            ))}
          </ol>
        )}
      </section>

      {r.canReply ? <Reply r={r} onSent={setNews} /> : <p className="portal-note">{t('This request is closed for answers.')}</p>}
      {r.canResolve && <Resolve r={r} onDone={setNews} />}
    </article>
  )
}

function Reply({ r, onSent }: { r: Detail; onSent: (news: string) => void }) {
  const qc = useQueryClient()
  const [text, setText] = useState('')
  const [files, setFiles] = useState<File[]>([])
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const input = useRef<HTMLInputElement>(null)
  const total = files.reduce((n, f) => n + f.size, 0)
  const tooMany = files.length > r.limits.files
  const tooBig = total > r.limits.mb * 1024 * 1024
  const problem = tooMany
    ? t('That’s more than {count} files: send the others with another message.', { count: r.limits.files })
    : tooBig
      ? t('Together the files are larger than {mb} MB.', { mb: r.limits.mb })
      : err
  return (
    <section className="portal-section" aria-labelledby="portal-reply-h">
      <h2 id="portal-reply-h">{t('Write to us')}</h2>
      <form
        className="portal-card"
        onSubmit={async (e) => {
          e.preventDefault()
          if (!text.trim() && !files.length) {
            setErr(t('Write a message or add a file.'))
            return
          }
          setBusy(true)
          setErr('')
          try {
            const fd = new FormData()
            fd.append('text', text)
            for (const f of files) fd.append('files', f, f.name)
            await api.upload(`/portal/requests/${r.key}/comments`, fd)
            setText('')
            setFiles([])
            if (input.current) input.current.value = ''
            onSent(t('Your message was sent.'))
            await qc.invalidateQueries({ queryKey: ['portal-request', r.key] })
            qc.invalidateQueries({ queryKey: ['portal-requests'] })
          } catch (e) {
            setErr(errText(e))
          } finally {
            setBusy(false)
          }
        }}
      >
        <div className="field">
          <label htmlFor="portal-reply-text">{t('Your message')}</label>
          <textarea id="portal-reply-text" className="textarea" rows={5} maxLength={r.limits.text} value={text} onChange={(e) => setText(e.target.value)} />
        </div>
        <div className="field">
          <label htmlFor="portal-reply-files">{t('Add files')}</label>
          <input
            id="portal-reply-files"
            ref={input}
            className="portal-file-input"
            type="file"
            multiple
            aria-describedby="portal-reply-files-hint"
            onChange={(e) => setFiles(Array.from(e.target.files ?? []))}
          />
          <span className="field-hint" id="portal-reply-files-hint">{t('Up to {count} files, {mb} MB together.', { count: r.limits.files, mb: r.limits.mb })}</span>
        </div>
        {problem && <p className="portal-error" role="alert">{problem}</p>}
        <button className="btn btn-primary" disabled={busy || tooMany || tooBig}>{busy ? t('Sending…') : t('Send')}</button>
      </form>
    </section>
  )
}

function Resolve({ r, onDone }: { r: Detail; onDone: (news: string) => void }) {
  const qc = useQueryClient()
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  return (
    <section className="portal-section portal-resolve" aria-labelledby="portal-resolve-h">
      <h2 id="portal-resolve-h">{t('Solved?')}</h2>
      <p className="portal-faint">{t('If everything is sorted out, let us know. You can still write to us afterwards.')}</p>
      {err && <p className="portal-error" role="alert">{err}</p>}
      <button
        type="button"
        className="btn"
        disabled={busy}
        onClick={async () => {
          setBusy(true)
          setErr('')
          try {
            await api.post(`/portal/requests/${r.key}/resolve`, {})
            onDone(t('Marked as resolved. Thank you!'))
            await qc.invalidateQueries({ queryKey: ['portal-request', r.key] })
            qc.invalidateQueries({ queryKey: ['portal-requests'] })
          } catch (e) {
            setErr(errText(e))
            setBusy(false)
          }
        }}
      >
        <CircleCheck size={15} aria-hidden /> {t('Mark as resolved')}
      </button>
    </section>
  )
}

// ---- request forms -----------------------------------------------------------------------------

function FormPage({ info }: { info: Info }) {
  const { path = '' } = useParams()
  const q = useQuery({
    queryKey: ['portal-form', path.toLowerCase()],
    queryFn: () => api.get<FormDef>(`/portal/forms/${encodeURIComponent(path)}`),
    retry: false,
    staleTime: Infinity,
    gcTime: 0,
  })
  const [sent, setSent] = useState<{ request: string | null } | null>(null)
  const title = q.data?.title || t('Submit a request')
  useTitle(isStatus(q.error, 404) ? t('Not found') : title)
  if (q.isLoading) return <Busy />
  if (isStatus(q.error, 404))
    return (
      <>
        <h1>{t('This form isn’t available')}</h1>
        <p className="portal-lead">{t('It may have been switched off or moved.')} <Link to="/portal">{t('Go to the start page')}</Link></p>
      </>
    )
  if (q.error || !q.data) return <Problem error={q.error} />
  const f = q.data
  return (
    <>
      <h1>{title}</h1>
      <p className="portal-sub">{f.project}</p>
      {sent ? (
        <Sent
          f={f}
          request={sent.request}
          onAgain={() => {
            setSent(null)
            void q.refetch()
          }}
        />
      ) : (
        <>
          {f.intro && <Paragraphs text={f.intro} className="portal-lead" />}
          <RequestForm key={q.dataUpdatedAt} f={f} info={info} openedAt={q.dataUpdatedAt} onSent={(r) => setSent(r)} />
        </>
      )}
    </>
  )
}

function Sent({ f, request, onAgain }: { f: FormDef; request: string | null; onAgain: () => void }) {
  const ref = useFocusOnMount<HTMLDivElement>()
  return (
    <div className="portal-card portal-done" ref={ref} tabIndex={-1}>
      <h2><CircleCheck size={18} aria-hidden /> {t('Request sent')}</h2>
      <Paragraphs text={f.success || t('Thanks! We’ve received your request and sent you a confirmation by e-mail.')} />
      <div className="portal-actions">
        {request && <Link className="btn btn-primary" to={`/portal/requests/${request}`}>{t('View your request {key}', { key: request })}</Link>}
        <button type="button" className="btn" onClick={onAgain}>{t('Send another request')}</button>
      </div>
    </div>
  )
}

/** "Name (optional)" for fields that may stay empty. */
const optional = (label: string, required: boolean) => (required ? label : t('{label} (optional)', { label }))

function RequestForm({ f, info, openedAt, onSent }: { f: FormDef; info: Info; openedAt: number; onSent: (r: { request: string | null }) => void }) {
  const [email, setEmail] = useState('')
  const [name, setName] = useState('')
  const [summary, setSummary] = useState('')
  const [description, setDescription] = useState('')
  const [typeId, setTypeId] = useState(f.types[0]?.id ?? '')
  const [priority, setPriority] = useState('medium')
  const [custom, setCustom] = useState<Record<string, unknown>>({})
  const [website, setWebsite] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const errRef = useRef<HTMLParagraphElement>(null)
  useEffect(() => {
    if (err) errRef.current?.focus()
  }, [err])
  const submit = async (e: FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    // people take longer than the form's minimum fill time; an autofilled form waits it out
    const wait = f.minSeconds * 1000 + 300 - (Date.now() - openedAt)
    if (wait > 0) await new Promise((res) => setTimeout(res, wait))
    try {
      const r = await api.post<{ ok: boolean; request: string | null }>(`/portal/forms/${encodeURIComponent(f.path)}`, {
        stamp: f.stamp,
        email: info.email ? undefined : email.trim(),
        name,
        summary,
        description,
        typeId: f.types.length ? typeId : undefined,
        priority: f.priority ? priority : undefined,
        custom,
        website,
      })
      onSent(r)
    } catch (e) {
      setErr(errText(e))
      setBusy(false)
    }
  }
  return (
    <form className="portal-card portal-form" onSubmit={submit}>
      {info.email ? (
        <p className="portal-note">{t('You’re signed in as {email}: we’ll answer to this address.', { email: info.email })}</p>
      ) : (
        <div className="field">
          <label htmlFor="rf-email">{t('Your e-mail address')}</label>
          <input id="rf-email" className="input" type="email" required autoComplete="email" maxLength={254} value={email} aria-describedby="rf-email-hint" onChange={(e) => setEmail(e.target.value)} />
          <span className="field-hint" id="rf-email-hint">{t('We send a confirmation and our answers there.')}</span>
        </div>
      )}
      {f.name !== 'off' && (
        <div className="field">
          <label htmlFor="rf-name">{optional(t('Your name'), f.name === 'required')}</label>
          <input id="rf-name" className="input" required={f.name === 'required'} autoComplete="name" maxLength={100} value={name} onChange={(e) => setName(e.target.value)} />
        </div>
      )}
      <div className="field">
        <label htmlFor="rf-summary">{t('Summary')}</label>
        <input id="rf-summary" className="input" required maxLength={200} value={summary} aria-describedby="rf-summary-hint" onChange={(e) => setSummary(e.target.value)} />
        <span className="field-hint" id="rf-summary-hint">{t('In a few words: what is it about?')}</span>
      </div>
      {f.types.length > 1 && (
        <div className="field">
          <label htmlFor="rf-type">{t('Kind of request')}</label>
          <select id="rf-type" className="select" required value={typeId} onChange={(e) => setTypeId(e.target.value)}>
            {f.types.map((ty) => <option key={ty.id} value={ty.id}>{ty.name}</option>)}
          </select>
        </div>
      )}
      {f.priority && (
        <div className="field">
          <label htmlFor="rf-priority">{t('How urgent is it?')}</label>
          <select id="rf-priority" className="select" value={priority} onChange={(e) => setPriority(e.target.value)}>
            <option value="low">{t('Low')}</option>
            <option value="medium">{t('Medium')}</option>
            <option value="high">{t('High')}</option>
            <option value="urgent">{t('Urgent')}</option>
          </select>
        </div>
      )}
      {f.description !== 'off' && (
        <div className="field">
          <label htmlFor="rf-description">{optional(t('Description'), f.description === 'required')}</label>
          <textarea
            id="rf-description"
            className="textarea"
            rows={6}
            required={f.description === 'required'}
            maxLength={20000}
            value={description}
            aria-describedby="rf-description-hint"
            onChange={(e) => setDescription(e.target.value)}
          />
          <span className="field-hint" id="rf-description-hint">{t('What happened, and what did you expect? The more we know, the sooner we can help.')}</span>
        </div>
      )}
      {f.fields.map((field) => (
        <CustomInput key={field.id} field={field} value={custom[field.id]} onChange={(v) => setCustom((c) => ({ ...c, [field.id]: v }))} />
      ))}
      {/* a trap for bots: hidden from people and assistive technology, filled in only by machines */}
      <div className="portal-hp" aria-hidden="true">
        <label htmlFor="rf-website">{t('Leave this field empty')}</label>
        <input id="rf-website" name="website" type="text" tabIndex={-1} autoComplete="off" value={website} onChange={(e) => setWebsite(e.target.value)} />
      </div>
      {err && <p className="portal-error" role="alert" tabIndex={-1} ref={errRef}>{err}</p>}
      <button className="btn btn-primary" disabled={busy}>{busy ? t('Sending…') : t('Send request')}</button>
    </form>
  )
}

function CustomInput({ field, value, onChange }: { field: Field; value: unknown; onChange: (v: unknown) => void }) {
  const id = `rf-cf-${field.id}`
  const hintId = field.description ? `${id}-hint` : undefined
  const label = optional(field.name, field.required)
  const hint: ReactNode = field.description && <span className="field-hint" id={hintId}>{field.description}</span>
  switch (field.kind) {
    case 'checkbox':
      return (
        <div className="field">
          <label className="portal-check">
            <input type="checkbox" required={field.required} checked={value === true} aria-describedby={hintId} onChange={(e) => onChange(e.target.checked)} />
            {label}
          </label>
          {hint}
        </div>
      )
    case 'select':
      return (
        <div className="field">
          <label htmlFor={id}>{label}</label>
          <select id={id} className="select" required={field.required} value={(value as string) ?? ''} aria-describedby={hintId} onChange={(e) => onChange(e.target.value || null)}>
            <option value="">{t('Choose…')}</option>
            {field.options.map((o) => <option key={o.id} value={o.id}>{o.label}</option>)}
          </select>
          {hint}
        </div>
      )
    case 'multiselect': {
      const list = (value as string[] | undefined) ?? []
      return (
        <fieldset className="field portal-fieldset" aria-describedby={hintId}>
          <legend>{label}</legend>
          {field.options.map((o) => (
            <label key={o.id} className="portal-check">
              <input type="checkbox" checked={list.includes(o.id)} onChange={(e) => onChange(e.target.checked ? [...list, o.id] : list.filter((x) => x !== o.id))} />
              {o.label}
            </label>
          ))}
          {hint}
        </fieldset>
      )
    }
    default: {
      const type = field.kind === 'number' ? 'number' : field.kind === 'date' ? 'date' : field.kind === 'url' ? 'url' : 'text'
      return (
        <div className="field">
          <label htmlFor={id}>{label}</label>
          <input
            id={id}
            className="input"
            type={type}
            step={field.kind === 'number' ? 'any' : undefined}
            placeholder={field.kind === 'url' ? 'https://' : undefined}
            maxLength={type === 'text' || type === 'url' ? 2000 : undefined}
            required={field.required}
            value={(value as string) ?? ''}
            aria-describedby={hintId}
            onChange={(e) => onChange(e.target.value)}
          />
          {hint}
        </div>
      )
    }
  }
}
