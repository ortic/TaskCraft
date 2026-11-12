import { t, AVAILABLE, LANGUAGES, chooseLanguage, locale, rememberLanguage } from '../i18n'
import { useEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Bell, BookOpen, Check, Contrast, Copy, ExternalLink, KeyRound, Lock, Monitor, Moon, Palette, ShieldCheck, Sun, Trash, User } from 'lucide-react'
import { api, ago, shortDate, type Me } from '../api'
import { Avatar, Confirm, ErrorBox, Loading, useMe, useToast } from '../ui'
import { providerName, type SignInMethods } from '../components/Sso'
import { SecuritySettings } from '../components/Security'
import './Settings.css'

// ---- small shared pieces (Admin uses them too) ----

/** Copies text to the clipboard; falls back to selecting `input` and the legacy copy command. */
export async function copyText(text: string, input?: HTMLInputElement | null): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    if (!input) return false
    input.focus()
    input.select()
    try {
      return document.execCommand('copy')
    } catch {
      return false
    }
  }
}

/** A read-only value (token, secret, link) with a copy button. */
export function CopyField({ value, label }: { value: string; label: string }) {
  const ref = useRef<HTMLInputElement>(null)
  const [copied, setCopied] = useState(false)
  useEffect(() => {
    if (!copied) return
    const h = setTimeout(() => setCopied(false), 1800)
    return () => clearTimeout(h)
  }, [copied])
  return (
    <div className="st-copy">
      <input ref={ref} className="input st-copy-input" readOnly value={value} aria-label={label} onFocus={(e) => e.currentTarget.select()} spellCheck={false} />
      <button type="button" className="btn" onClick={async () => setCopied(await copyText(value, ref.current))}>
        {copied ? <Check size={15} /> : <Copy size={15} />} {copied ? t('Copied') : t('Copy')}
      </button>
    </div>
  )
}

/** An on/off switch (a styled checkbox). */
export function Switch({ checked, onChange, label, disabled }: { checked: boolean; onChange: (on: boolean) => void; label: string; disabled?: boolean }) {
  return <input type="checkbox" role="switch" className="st-switch" checked={checked} disabled={disabled} aria-label={label} title={label} onChange={(e) => onChange(e.target.checked)} />
}

function Section({ id, icon, title, desc, children, footer }: { id: string; icon: ReactNode; title: string; desc?: ReactNode; children: ReactNode; footer?: ReactNode }) {
  return (
    <section className="st-card" id={`st-${id}`} aria-labelledby={`st-${id}-title`}>
      <div className="st-card-head">
        <h2 id={`st-${id}-title`}>{icon} {title}</h2>
        {desc && <p className="st-desc">{desc}</p>}
      </div>
      <div className="st-card-body">{children}</div>
      {footer && <div className="st-card-foot">{footer}</div>}
    </section>
  )
}

const SECTIONS = [
  { id: 'profile', icon: <User size={15} />, label: () => t('Profile') },
  { id: 'password', icon: <Lock size={15} />, label: () => t('Password') },
  { id: 'security', icon: <ShieldCheck size={15} />, label: () => t('Security') },
  { id: 'appearance', icon: <Palette size={15} />, label: () => t('Appearance') },
  { id: 'notifications', icon: <Bell size={15} />, label: () => t('Notifications') },
  { id: 'tokens', icon: <KeyRound size={15} />, label: () => t('API tokens') },
  { id: 'wikicraft', icon: <BookOpen size={15} />, label: () => t('WikiCraft') },
]

function scrollToSection(id: string) {
  document.getElementById(`st-${id}`)?.scrollIntoView({ behavior: 'smooth', block: 'start' })
}

export default function Settings() {
  const me = useMe()
  const [preset, setPreset] = useState<{ name: string; scope: 'read' | 'write'; n: number } | null>(null)
  if (me.isLoading) return <Loading />
  if (me.error) return <div className="page"><ErrorBox error={me.error} /></div>
  const data = me.data
  if (!data?.user) return null
  return (
    <div className="page st-page">
      <div className="page-head">
        <h1>{t('Profile & settings')}</h1>
      </div>
      <div className="st-layout">
        <nav className="st-nav" aria-label={t('Settings sections')}>
          {SECTIONS.map((s) => (
            <button key={s.id} type="button" className="st-nav-item" onClick={() => scrollToSection(s.id)}>
              {s.icon} <span>{s.label()}</span>
            </button>
          ))}
        </nav>
        <div className="st-sections">
          <ProfileSection me={data} />
          <PasswordSection />
          <Section id="security" icon={<ShieldCheck size={16} />} title={t('Security')} desc={t('Two-factor authentication, passkeys and where you’re signed in.')}>
            <SecuritySettings />
          </Section>
          <AppearanceSection />
          <NotificationsSection me={data} />
          <TokensSection me={data} preset={preset} />
          <WikiSection
            me={data}
            onCreateToken={() => {
              setPreset((p) => ({ name: 'WikiCraft', scope: 'read', n: (p?.n ?? 0) + 1 }))
              scrollToSection('tokens')
            }}
          />
        </div>
      </div>
    </div>
  )
}

// ---- profile ----

function timeZones(current: string): string[] {
  let zones: string[] = []
  try {
    zones = Intl.supportedValuesOf('timeZone')
  } catch {
    /* old browser */
  }
  if (current && !zones.includes(current)) zones = [current, ...zones]
  return zones
}

function nowIn(tz: string): string {
  try {
    return new Intl.DateTimeFormat(locale(), { timeZone: tz || undefined, weekday: 'short', hour: 'numeric', minute: '2-digit' }).format(new Date())
  } catch {
    return ''
  }
}

function ProfileSection({ me }: { me: Me }) {
  const user = me.user!
  const qc = useQueryClient()
  const toast = useToast()
  const [name, setName] = useState(user.name)
  const [title, setTitle] = useState(me.profile?.title ?? '')
  const [lang, setLang] = useState(me.profile?.locale ?? '')
  const [tz, setTz] = useState(me.profile?.timezone ?? '')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const zones = useMemo(() => timeZones(me.profile?.timezone ?? ''), [me.profile?.timezone])
  const browserTz = useMemo(() => Intl.DateTimeFormat().resolvedOptions().timeZone, [])
  const wsLang = LANGUAGES.find((l) => l.code === me.workspace?.defaultLanguage)
  const dirty = name.trim() !== user.name || title.trim() !== (me.profile?.title ?? '') || lang !== (me.profile?.locale ?? '') || tz !== (me.profile?.timezone ?? '')

  const save = async (e: FormEvent) => {
    e.preventDefault()
    if (!name.trim()) return setErr(t('Your name can’t be empty.'))
    setBusy(true)
    setErr('')
    try {
      await api.put('/me/profile', { name: name.trim(), title: title.trim(), locale: lang, timezone: tz })
      const next = chooseLanguage(lang || null, me.workspace?.defaultLanguage)
      if (next !== locale()) {
        rememberLanguage(next)
        window.location.reload()
        return
      }
      await qc.invalidateQueries({ queryKey: ['me'] })
      qc.invalidateQueries({ queryKey: ['people'] })
      toast(t('Profile saved'))
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <form onSubmit={save}>
      <Section
        id="profile"
        icon={<User size={16} />}
        title={t('Profile')}
        desc={t('How you appear to others in {workspace}.', { workspace: me.workspace?.name ?? 'TaskCraft' })}
        footer={
          <>
            {err && <span className="error-text st-foot-msg">{err}</span>}
            <button type="submit" className="btn btn-primary" disabled={busy || !dirty}>{t('Save profile')}</button>
          </>
        }
      >
        <div className="st-identity">
          <Avatar name={name.trim() || user.name} id={user.id} size={48} />
          <div className="st-identity-text">
            <div className="st-identity-name">{name.trim() || user.name}</div>
            <div className="faint small">{user.email}</div>
          </div>
        </div>
        <div className="st-grid2">
          <div className="field">
            <label htmlFor="st-name">{t('Name')}</label>
            <input id="st-name" className="input" value={name} onChange={(e) => setName(e.target.value)} autoComplete="name" maxLength={100} required />
          </div>
          <div className="field">
            <label htmlFor="st-title">{t('Job title')}</label>
            <input id="st-title" className="input" value={title} onChange={(e) => setTitle(e.target.value)} placeholder={t('e.g. Product designer')} autoComplete="organization-title" maxLength={100} />
          </div>
          <div className="field">
            <label htmlFor="st-lang">{t('Language')}</label>
            <select id="st-lang" className="select" value={lang} onChange={(e) => setLang(e.target.value)}>
              <option value="">{wsLang ? t('Workspace default ({language})', { language: wsLang.name }) : t('Automatic (browser language)')}</option>
              {AVAILABLE.map((l) => (
                <option key={l.code} value={l.code} lang={l.code}>{l.name}</option>
              ))}
              {lang && !AVAILABLE.some((l) => l.code === lang) && <option value={lang}>{LANGUAGES.find((l) => l.code === lang)?.name ?? lang}</option>}
            </select>
            <span className="field-hint">{t('The page reloads in the new language when you save.')}</span>
          </div>
          <div className="field">
            <label htmlFor="st-tz">{t('Time zone')}</label>
            <select id="st-tz" className="select" value={tz} onChange={(e) => setTz(e.target.value)}>
              <option value="">{t('Automatic ({zone})', { zone: browserTz })}</option>
              {zones.map((z) => (
                <option key={z} value={z}>{z.replace(/_/g, ' ')}</option>
              ))}
            </select>
            <span className="field-hint">{t('Local time there: {time}', { time: nowIn(tz) })}</span>
          </div>
        </div>
      </Section>
    </form>
  )
}

// ---- password ----

function PasswordSection() {
  const toast = useToast()
  const [current, setCurrent] = useState('')
  const [next, setNext] = useState('')
  const [again, setAgain] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const mismatch = again.length > 0 && next !== again
  // single sign-on: people without a password (or all, when passwords are off) can't change one
  const signIn = useQuery({ queryKey: ['me', 'sign-in'], queryFn: () => api.get<SignInMethods & { passwordLogin: boolean }>('/me/sign-in') })
  const save = async (e: FormEvent) => {
    e.preventDefault()
    if (next.length < 8) return setErr(t('The new password must be at least 8 characters.'))
    if (next !== again) return setErr(t('The new passwords don’t match.'))
    setBusy(true)
    setErr('')
    try {
      await api.put('/me/password', { current, newPassword: next })
      setCurrent('')
      setNext('')
      setAgain('')
      toast(t('Password changed. Your other sessions were signed out.'))
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  if (signIn.data && (!signIn.data.passwordLogin || !signIn.data.password)) {
    const providers = signIn.data.providers.map(providerName).join(', ')
    return (
      <Section id="password" icon={<Lock size={16} />} title={t('Password')}>
        <p className="muted" style={{ margin: 0 }}>
          {signIn.data.passwordLogin ? t('Your account has no password.') : t('Password sign-in is turned off for this workspace.')}{' '}
          {t('You sign in with {providers}.', { providers: providers || t('single sign-on') })}
        </p>
      </Section>
    )
  }
  return (
    <form onSubmit={save}>
      <Section
        id="password"
        icon={<Lock size={16} />}
        title={t('Password')}
        desc={t('Changing your password signs you out everywhere else.')}
        footer={
          <>
            {err && <span className="error-text st-foot-msg" role="alert">{err}</span>}
            <button type="submit" className="btn btn-primary" disabled={busy || !current || !next || !again}>{t('Change password')}</button>
          </>
        }
      >
        <div className="st-grid3">
          <div className="field">
            <label htmlFor="st-pw-cur">{t('Current password')}</label>
            <input id="st-pw-cur" type="password" className="input" value={current} onChange={(e) => setCurrent(e.target.value)} autoComplete="current-password" />
          </div>
          <div className="field">
            <label htmlFor="st-pw-new">{t('New password')}</label>
            <input id="st-pw-new" type="password" className="input" value={next} onChange={(e) => setNext(e.target.value)} autoComplete="new-password" minLength={8} />
            <span className="field-hint">{t('At least 8 characters.')}</span>
          </div>
          <div className="field">
            <label htmlFor="st-pw-again">{t('Repeat new password')}</label>
            <input id="st-pw-again" type="password" className={`input ${mismatch ? 'st-invalid' : ''}`} value={again} onChange={(e) => setAgain(e.target.value)} autoComplete="new-password" aria-invalid={mismatch} />
            {mismatch && <span className="field-hint st-hint-danger">{t('Doesn’t match yet.')}</span>}
          </div>
        </div>
      </Section>
    </form>
  )
}

// ---- appearance ----

type Theme = 'light' | 'dark' | 'contrast' | 'system'

function readTheme(): Theme {
  const v = document.documentElement.dataset.theme
  return v === 'light' || v === 'dark' || v === 'contrast' ? v : 'system'
}

function applyTheme(theme: Theme) {
  const root = document.documentElement
  try {
    if (theme === 'system') {
      delete root.dataset.theme
      localStorage.removeItem('tc-theme')
    } else {
      root.dataset.theme = theme
      localStorage.setItem('tc-theme', theme)
    }
  } catch {
    /* storage unavailable: the choice lasts until the page reloads */
  }
}

const THEMES: { id: Theme; icon: ReactNode; label: () => string; desc: () => string }[] = [
  { id: 'light', icon: <Sun size={18} />, label: () => t('Light'), desc: () => t('Dark text on a light background.') },
  { id: 'dark', icon: <Moon size={18} />, label: () => t('Dark'), desc: () => t('Easier on the eyes in dim light.') },
  { id: 'contrast', icon: <Contrast size={18} />, label: () => t('High contrast'), desc: () => t('Stronger text, borders and focus rings.') },
  { id: 'system', icon: <Monitor size={18} />, label: () => t('Follow system'), desc: () => t('Light or dark, like your device.') },
]

function AppearanceSection() {
  const [theme, setTheme] = useState<Theme>(readTheme)
  // the top bar's theme toggle changes it too
  useEffect(() => {
    const obs = new MutationObserver(() => setTheme(readTheme()))
    obs.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] })
    return () => obs.disconnect()
  }, [])
  return (
    <Section id="appearance" icon={<Palette size={16} />} title={t('Appearance')} desc={t('Saved in this browser only.')}>
      <div className="st-themes" role="radiogroup" aria-label={t('Theme')}>
        {THEMES.map((th) => (
          <button
            key={th.id}
            type="button"
            role="radio"
            aria-checked={theme === th.id}
            className="st-theme"
            onClick={() => {
              applyTheme(th.id)
              setTheme(th.id)
            }}
          >
            <span className="st-theme-top">
              {th.icon}
              <span className="st-theme-check">{theme === th.id && <Check size={14} />}</span>
            </span>
            <span className="st-theme-label">{th.label()}</span>
            <span className="faint small">{th.desc()}</span>
          </button>
        ))}
      </div>
    </Section>
  )
}

// ---- notifications ----

const KINDS: { id: string; label: () => string; desc: () => string }[] = [
  { id: 'assigned', label: () => t('Assigned to you'), desc: () => t('Someone makes you the assignee of an issue.') },
  { id: 'mentioned', label: () => t('Mentions'), desc: () => t('Someone @mentions you in a description or comment.') },
  { id: 'commented', label: () => t('Comments'), desc: () => t('New comments on issues you watch.') },
  { id: 'status', label: () => t('Status changes'), desc: () => t('Issues you watch move to another status.') },
]

function NotificationsSection({ me }: { me: Me }) {
  const qc = useQueryClient()
  const toast = useToast()
  const saved: string[] = Array.isArray(me.preferences?.muted) ? me.preferences!.muted : []
  const [muted, setMuted] = useState<string[]>(saved)
  useEffect(() => setMuted(saved), [saved.join(',')]) // eslint-disable-line react-hooks/exhaustive-deps
  const toggle = async (kind: string, on: boolean) => {
    const next = on ? muted.filter((k) => k !== kind) : [...muted.filter((k) => k !== kind), kind]
    const before = muted
    setMuted(next)
    try {
      await api.put('/me/preferences', { muted: next })
      qc.invalidateQueries({ queryKey: ['me'] })
    } catch (e) {
      setMuted(before)
      toast((e as Error).message)
    }
  }
  return (
    <Section id="notifications" icon={<Bell size={16} />} title={t('Notifications')} desc={t('Choose what shows up in your notification inbox. You never get notified about your own changes.')}>
      <div className="st-toggles">
        {KINDS.map((k) => {
          const on = !muted.includes(k.id)
          return (
            <label key={k.id} className="st-toggle">
              <span className="st-toggle-text">
                <span className="st-toggle-label">{k.label()}</span>
                <span className="faint small">{k.desc()}</span>
              </span>
              <Switch checked={on} onChange={(v) => toggle(k.id, v)} label={k.label()} />
            </label>
          )
        })}
      </div>
    </Section>
  )
}

// ---- API tokens ----

interface ApiToken {
  id: string
  name: string
  scope: 'read' | 'write'
  createdAt: string
  expiresAt: string | null
  lastUsedAt: string | null
}

const EXPIRY = [
  { days: 30, label: () => t('30 days') },
  { days: 90, label: () => t('90 days') },
  { days: 365, label: () => t('1 year') },
  { days: 0, label: () => t('Never') },
]

function TokensSection({ me, preset }: { me: Me; preset: { name: string; scope: 'read' | 'write'; n: number } | null }) {
  const qc = useQueryClient()
  const tokens = useQuery({ queryKey: ['my-tokens'], queryFn: () => api.get<ApiToken[]>('/me/tokens') })
  const [name, setName] = useState('')
  const [scope, setScope] = useState<'read' | 'write'>('read')
  const [days, setDays] = useState(90)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const [created, setCreated] = useState<{ name: string; token: string; scope: string } | null>(null)
  const [revoking, setRevoking] = useState<ApiToken | null>(null)
  const nameRef = useRef<HTMLInputElement>(null)
  useEffect(() => {
    if (!preset) return
    setName(preset.name)
    setScope(preset.scope)
    setDays(0)
    nameRef.current?.focus({ preventScroll: true })
  }, [preset])

  const create = async (e: FormEvent) => {
    e.preventDefault()
    if (!name.trim()) return setErr(t('Give the token a name, e.g. what uses it.'))
    setBusy(true)
    setErr('')
    try {
      const r = await api.post<{ id: string; token: string }>('/me/tokens', { name: name.trim(), scope, expiresInDays: days || undefined })
      setCreated({ name: name.trim(), token: r.token, scope })
      setName('')
      qc.invalidateQueries({ queryKey: ['my-tokens'] })
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  const base = me.baseUrl ?? window.location.origin
  const list = tokens.data ?? []
  return (
    <Section
      id="tokens"
      icon={<KeyRound size={16} />}
      title={t('API tokens')}
      desc={t('Tokens let scripts and other tools use the TaskCraft API as you. A read-only token can look but not change anything.')}
    >
      <form className="st-token-form" onSubmit={create}>
        <div className="field st-token-name">
          <label htmlFor="st-tok-name">{t('Token name')}</label>
          <input id="st-tok-name" ref={nameRef} className="input" value={name} onChange={(e) => setName(e.target.value)} placeholder={t('e.g. CI pipeline')} maxLength={100} />
        </div>
        <div className="field">
          <span className="field-label" id="st-tok-scope">{t('Access')}</span>
          <div className="seg" role="radiogroup" aria-labelledby="st-tok-scope">
            <button type="button" role="radio" aria-checked={scope === 'read'} className={scope === 'read' ? 'active' : ''} onClick={() => setScope('read')}>{t('Read only')}</button>
            <button type="button" role="radio" aria-checked={scope === 'write'} className={scope === 'write' ? 'active' : ''} onClick={() => setScope('write')}>{t('Read & write')}</button>
          </div>
        </div>
        <div className="field">
          <label htmlFor="st-tok-exp">{t('Expires')}</label>
          <select id="st-tok-exp" className="select" value={days} onChange={(e) => setDays(Number(e.target.value))}>
            {EXPIRY.map((x) => <option key={x.days} value={x.days}>{x.label()}</option>)}
          </select>
        </div>
        <div className="field st-token-submit">
          <button type="submit" className="btn btn-primary" disabled={busy}>{t('Create token')}</button>
        </div>
      </form>
      {err && <p className="error-text" role="alert">{err}</p>}

      {created && (
        <div className="st-reveal" role="status">
          <div className="st-reveal-head">
            <Check size={16} /> <strong>{t('Token “{name}” created', { name: created.name })}</strong>
            <span className="spacer" />
            <button type="button" className="btn btn-sm btn-ghost" onClick={() => setCreated(null)}>{t('Done [[close]]')}</button>
          </div>
          <p className="small">{t('Copy it now: it’s shown only once. Anyone who has it can act as you, so keep it secret.')}</p>
          <CopyField value={created.token} label={t('New API token')} />
          <p className="small muted">{t('Send it in the Authorization header of each request:')}</p>
          <pre className="st-code">{`curl -H "Authorization: Bearer ${created.token}" \\\n  ${base}/api/me`}</pre>
        </div>
      )}

      {tokens.isLoading && <Loading />}
      {tokens.error && <ErrorBox error={tokens.error} />}
      {tokens.data && list.length === 0 && <p className="faint small st-none">{t('You have no API tokens yet.')}</p>}
      {list.length > 0 && (
        <div className="st-table-wrap">
          <table className="list st-tokens">
            <thead>
              <tr>
                <th>{t('Name')}</th>
                <th>{t('Access')}</th>
                <th>{t('Created')}</th>
                <th>{t('Expires')}</th>
                <th>{t('Last used')}</th>
                <th><span className="sr-only">{t('Actions')}</span></th>
              </tr>
            </thead>
            <tbody>
              {list.map((tk) => {
                const expired = !!tk.expiresAt && new Date(tk.expiresAt) < new Date()
                return (
                  <tr key={tk.id}>
                    <td className="st-tok-cell-name">{tk.name}</td>
                    <td><span className={`badge ${tk.scope === 'write' ? 'badge-warning' : ''}`}>{tk.scope === 'write' ? t('Read & write') : t('Read only')}</span></td>
                    <td className="small">{shortDate(tk.createdAt)}</td>
                    <td className="small">{expired ? <span className="badge badge-danger">{t('Expired')}</span> : tk.expiresAt ? shortDate(tk.expiresAt) : <span className="faint">{t('Never')}</span>}</td>
                    <td className="small">{tk.lastUsedAt ? ago(tk.lastUsedAt) : <span className="faint">{t('Never')}</span>}</td>
                    <td className="st-tok-actions">
                      <button type="button" className="btn btn-sm btn-danger" onClick={() => setRevoking(tk)}><Trash size={14} /> {t('Revoke')}</button>
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      )}
      {revoking && (
        <Confirm
          title={t('Revoke token “{name}”?', { name: revoking.name })}
          body={<p>{t('Anything that uses this token stops working right away. This can’t be undone.')}</p>}
          confirmLabel={t('Revoke')}
          danger
          onClose={() => setRevoking(null)}
          onConfirm={async () => {
            await api.del(`/me/tokens/${revoking.id}`)
            qc.invalidateQueries({ queryKey: ['my-tokens'] })
          }}
        />
      )}
    </Section>
  )
}

// ---- WikiCraft ----

function WikiSection({ me, onCreateToken }: { me: Me; onCreateToken: () => void }) {
  const qc = useQueryClient()
  const toast = useToast()
  const wiki = me.wikicraft
  const [token, setToken] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const base = me.baseUrl ?? window.location.origin
  const refresh = async () => {
    await qc.invalidateQueries({ queryKey: ['me'] })
    for (const k of ['wiki-search', 'wiki-spaces', 'wiki-pages']) qc.invalidateQueries({ queryKey: [k] })
  }
  const connect = async (e: FormEvent) => {
    e.preventDefault()
    const v = token.trim()
    if (!v.startsWith('wct_')) return setErr(t('WikiCraft tokens start with wct_.'))
    setBusy(true)
    setErr('')
    try {
      await api.put('/me/preferences', { wikicraftToken: v })
      setToken('')
      await refresh()
      toast(t('WikiCraft connected with your token'))
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  const disconnect = async () => {
    setBusy(true)
    try {
      await api.put('/me/preferences', { wikicraftToken: null })
      await refresh()
      toast(t('Your WikiCraft token was removed'))
    } catch (e) {
      toast((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Section id="wikicraft" icon={<BookOpen size={16} />} title={t('WikiCraft')} desc={t('Link wiki pages to issues and show issues in the wiki.')}>
      {wiki?.url ? (
        <>
          <div className="st-wiki-status">
            {wiki.personal ? (
              <span className="badge badge-success"><Check size={12} /> {t('Connected with your token')}</span>
            ) : wiki.connected ? (
              <span className="badge badge-accent">{t('Connected through the workspace')}</span>
            ) : (
              <span className="badge badge-warning">{t('Not connected')}</span>
            )}
            <a href={wiki.url} target="_blank" rel="noreferrer" className="small st-wiki-link">{wiki.url.replace(/^https?:\/\//, '')} <ExternalLink size={12} /></a>
          </div>
          <p className="small muted">
            {wiki.personal
              ? t('Page titles, searches and links use your own WikiCraft token, so you only see pages your wiki permissions allow.')
              : wiki.connected
                ? t('TaskCraft reaches the wiki with the workspace’s shared token. Connect your own WikiCraft token so page titles and searches respect your own wiki permissions.')
                : t('Connect your WikiCraft token to search and link wiki pages from issues. With a personal token, page titles and searches respect your own wiki permissions.')}
          </p>
          {wiki.personal ? (
            <button type="button" className="btn btn-danger" disabled={busy} onClick={disconnect}><Trash size={14} /> {t('Remove my WikiCraft token')}</button>
          ) : (
            <form className="st-wiki-form" onSubmit={connect}>
              <div className="field st-wiki-input">
                <label htmlFor="st-wct">{t('Personal WikiCraft token')}</label>
                <input id="st-wct" type="password" className="input" value={token} onChange={(e) => setToken(e.target.value)} placeholder="wct_…" autoComplete="off" spellCheck={false} />
                <span className="field-hint">{t('Create one in WikiCraft under Profile → API tokens.')}</span>
              </div>
              <button type="submit" className="btn btn-primary" disabled={busy || !token.trim()}>{t('Connect')}</button>
            </form>
          )}
          {err && <p className="error-text" role="alert">{err}</p>}
        </>
      ) : (
        <p className="small muted">{t('This workspace isn’t linked to a WikiCraft wiki. An administrator can link one with TASKCRAFT_WIKICRAFT_URL and TASKCRAFT_WIKICRAFT_TOKEN.')}</p>
      )}

      <div className="st-subsection">
        <h3>{t('Show TaskCraft issues in WikiCraft')}</h3>
        <p className="small muted">
          {t('WikiCraft can show live issue keys and issue lists on wiki pages. Create a read-only TaskCraft token and give it to your WikiCraft administrator, who sets:')}
        </p>
        <pre className="st-code">{`WIKICRAFT_TASKCRAFT_URL=${base}\nWIKICRAFT_TASKCRAFT_TOKEN=<${t('your read-only token')}>`}</pre>
        <button type="button" className="btn" onClick={onCreateToken}><KeyRound size={14} /> {t('Create a read-only token')}</button>
      </div>
    </Section>
  )
}
