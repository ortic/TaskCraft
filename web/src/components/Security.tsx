// Account security: two-factor authentication (authenticator app, recovery codes), passkeys and
// sessions under Profile & settings → Security; the enrollment screen for workspaces that require
// a second factor; for admins the workspace policy and resetting someone's second factor.
import { t } from '../i18n'
import { useEffect, useId, useState, type FormEvent, type ReactNode } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Check, Copy, Download, Fingerprint, LogOut, Monitor, Pencil, ShieldCheck, Smartphone, Trash } from 'lucide-react'
import { ApiError, ago, api, dateTime, shortDate, type Me } from '../api'
import { cancelled, createPasskey, passkeysSupported } from '../passkeys'
import { Confirm, ErrorBox, Loading, Logo, Modal, useToast } from '../ui'
import './Security.css'

export interface MfaStatus {
  /** the authenticator app is on */
  enabled: boolean
  enabledAt: string | null
  recoveryCodesLeft: number
  hasPassword: boolean
  passkeys: number
  /** the workspace requires a second factor of this person */
  required: boolean
}

interface Passkey {
  id: string
  name: string
  /** synced to the person's other devices */
  backedUp: boolean
  createdAt: string
  lastUsedAt: string | null
}

interface Session {
  id: string
  createdAt: string
  lastSeenAt: string
  userAgent: string | null
  ip: string | null
  current: boolean
}

type Setup = { secret: string; uri: string; qrSvg: string }

const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e))

export const useMfaStatus = () => useQuery({ queryKey: ['mfa'], queryFn: () => api.get<MfaStatus>('/me/mfa') })

/** "Firefox on Windows", from a user agent. */
function device(ua: string | null): string {
  if (!ua) return t('Unknown device')
  const browser = /Edg\//.test(ua) ? 'Edge' : /Firefox\//.test(ua) ? 'Firefox' : /Chrome\//.test(ua) ? 'Chrome' : /Safari\//.test(ua) ? 'Safari' : /curl|reqwest|python|Go-http/i.test(ua) ? t('API client') : t('Browser')
  const os = /Windows/.test(ua) ? 'Windows' : /iPhone|iPad/.test(ua) ? 'iOS' : /Mac OS X|Macintosh/.test(ua) ? 'macOS' : /Android/.test(ua) ? 'Android' : /Linux/.test(ua) ? 'Linux' : ''
  return os ? t('{browser} on {os}', { browser, os }) : browser
}

// ---- asking for a password, a code or a name -------------------------------------------------

type Ask = {
  title: string
  intro?: ReactNode
  label: string
  kind: 'password' | 'code' | 'text'
  initial?: string
  confirmLabel: string
  danger?: boolean
  /** runs with what was entered; an error keeps the dialog open and shows its message */
  run: (value: string) => Promise<void>
}

function AskDialog({ ask, onClose }: { ask: Ask; onClose: () => void }) {
  const [value, setValue] = useState(ask.initial ?? '')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const id = useId()
  const submit = async (e?: FormEvent) => {
    e?.preventDefault()
    if (!value.trim() || busy) return
    setBusy(true)
    setErr('')
    try {
      await ask.run(ask.kind === 'password' ? value : value.trim())
      onClose()
    } catch (e) {
      if (cancelled(e)) return onClose()
      setErr(errorText(e))
      setBusy(false)
    }
  }
  return (
    <Modal
      title={ask.title}
      onClose={onClose}
      width={440}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className={`btn ${ask.danger ? 'btn-danger-solid' : 'btn-primary'}`} disabled={busy || !value.trim()} onClick={() => submit()}>{ask.confirmLabel}</button>
        </>
      }
    >
      <form onSubmit={submit}>
        {ask.intro && <p className="small muted" style={{ marginTop: 0 }}>{ask.intro}</p>}
        <div className="field">
          <label htmlFor={id}>{ask.label}</label>
          <input
            id={id}
            className={`input ${ask.kind === 'code' ? 'sec-otp' : ''}`}
            type={ask.kind === 'password' ? 'password' : 'text'}
            autoComplete={ask.kind === 'password' ? 'current-password' : ask.kind === 'code' ? 'one-time-code' : 'off'}
            spellCheck={false}
            autoFocus
            maxLength={ask.kind === 'text' ? 80 : undefined}
            value={value}
            onChange={(e) => setValue(e.target.value)}
          />
        </div>
        {err && <p className="error-text" role="alert">{err}</p>}
        <button type="submit" hidden />
      </form>
    </Modal>
  )
}

function useAsk() {
  const [ask, setAsk] = useState<Ask | null>(null)
  return { ask: (a: Ask) => setAsk(a), dialog: ask && <AskDialog key={ask.title} ask={ask} onClose={() => setAsk(null)} /> }
}

/**
 * Runs a change to sign-in methods. Signed in more than 15 minutes ago, the server wants the
 * password first: asks for it and runs the change again with it.
 */
async function withPassword(call: (password?: string) => Promise<void>, ask: (a: Ask) => void, hasPassword: boolean, title: string, confirmLabel: string) {
  try {
    await call()
  } catch (e) {
    if (!(e instanceof ApiError && e.status === 403)) throw e
    if (!hasPassword) throw new Error(t('Sign out and in again, then try once more.'))
    ask({ title, intro: t('You signed in more than 15 minutes ago. Confirm it’s you with your password.'), label: t('Password'), kind: 'password', confirmLabel, run: (password) => call(password) })
  }
}

// ---- recovery codes and setting up the app ---------------------------------------------------

function RecoveryCodes({ codes, onClose }: { codes: string[]; onClose: () => void }) {
  const [copied, setCopied] = useState(false)
  const text = codes.join('\n')
  const download = () => {
    const url = URL.createObjectURL(new Blob([`${t('TaskCraft recovery codes')}\n\n${text}\n`], { type: 'text/plain' }))
    const a = document.createElement('a')
    a.href = url
    a.download = 'taskcraft-recovery-codes.txt'
    a.click()
    setTimeout(() => URL.revokeObjectURL(url), 1000)
  }
  return (
    <Modal title={t('Save your recovery codes')} onClose={onClose} width={480} footer={<button className="btn btn-primary" onClick={onClose}>{t('I’ve saved them')}</button>}>
      <p className="small" style={{ marginTop: 0 }}>{t('Each code signs you in once when you can’t use your authenticator app. Keep them somewhere safe, like a password manager: they aren’t shown again.')}</p>
      <div className="sec-codes" aria-label={t('Recovery codes')} role="list">
        {codes.map((c) => <code key={c} role="listitem">{c}</code>)}
      </div>
      <div className="sec-actions">
        <button
          type="button"
          className="btn"
          onClick={async () => {
            try {
              await navigator.clipboard.writeText(text)
              setCopied(true)
            } catch {
              /* no clipboard: the codes can still be selected */
            }
          }}
        >
          {copied ? <Check size={14} /> : <Copy size={14} />} {copied ? t('Copied') : t('Copy')}
        </button>
        <button type="button" className="btn" onClick={download}><Download size={14} /> {t('Download')}</button>
      </div>
    </Modal>
  )
}

function EnableDialog({ setup, onDone, onClose }: { setup: Setup; onDone: (codes: string[]) => void; onClose: () => void }) {
  const [code, setCode] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const digits = code.replace(/\D/g, '')
  const confirm = async (e?: FormEvent) => {
    e?.preventDefault()
    if (digits.length !== 6 || busy) return
    setBusy(true)
    setErr('')
    try {
      const r = await api.post<{ recoveryCodes: string[] }>('/me/mfa/enable', { code: digits })
      onDone(r.recoveryCodes)
    } catch (e) {
      setErr(errorText(e))
      setBusy(false)
    }
  }
  return (
    <Modal
      title={t('Set up two-factor authentication')}
      onClose={onClose}
      width={620}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || digits.length !== 6} onClick={() => confirm()}>{t('Turn on')}</button>
        </>
      }
    >
      <form className="sec-setup" onSubmit={confirm}>
        {/* the server draws the QR code (an SVG without scripts) */}
        <div className="sec-qr" role="img" aria-label={t('QR code for your authenticator app')} dangerouslySetInnerHTML={{ __html: setup.qrSvg }} />
        <div>
          <ol className="sec-steps small">
            <li>{t('Scan the QR code with an authenticator app (1Password, Bitwarden, Google or Microsoft Authenticator, …).')}</li>
            <li>{t('Can’t scan it? Enter this key instead:')} <code className="sec-key">{setup.secret.replace(/(.{4})/g, '$1 ').trim()}</code></li>
            <li>{t('Type the 6-digit code the app shows.')}</li>
          </ol>
          <div className="field">
            <label htmlFor="sec-enable-code">{t('Code')}</label>
            <input id="sec-enable-code" className="input sec-otp" inputMode="numeric" autoComplete="one-time-code" value={code} onChange={(e) => setCode(e.target.value)} placeholder="123 456" maxLength={9} autoFocus />
          </div>
          {err && <p className="error-text" role="alert">{err}</p>}
          <button type="submit" hidden />
        </div>
      </form>
    </Modal>
  )
}

// ---- the three blocks ------------------------------------------------------------------------

/** The authenticator app and recovery codes. */
function AppBlock({ status, ask, onCodes, lastFactor }: { status: MfaStatus; ask: (a: Ask) => void; onCodes: (codes: string[]) => void; lastFactor: boolean }) {
  const qc = useQueryClient()
  const toast = useToast()
  const [setup, setSetup] = useState<Setup | null>(null)
  const refresh = () => qc.invalidateQueries({ queryKey: ['mfa'] })
  const start = async () => {
    try {
      await withPassword(
        async (password) => setSetup(await api.post<Setup>('/me/mfa/setup', password ? { password } : {})),
        ask,
        status.hasPassword,
        t('Set up two-factor authentication'),
        t('Continue'),
      )
    } catch (e) {
      toast(errorText(e))
    }
  }
  const turnOff = (value: string) => api.post('/me/mfa/disable', status.hasPassword ? { password: value } : { code: value }).then(() => {
    toast(t('Two-factor authentication is off'))
    refresh()
  })
  const disable = () =>
    ask({
      title: t('Turn off two-factor authentication?'),
      intro: t('Signing in then needs only your password (or a passkey).'),
      label: status.hasPassword ? t('Password') : t('Code from your authenticator app'),
      kind: status.hasPassword ? 'password' : 'code',
      confirmLabel: t('Turn off'),
      danger: true,
      run: turnOff,
    })
  const regenerate = () =>
    ask({
      title: t('New recovery codes'),
      intro: t('Your current recovery codes stop working. Confirm with a code from your authenticator app (or a recovery code).'),
      label: t('Code'),
      kind: 'code',
      confirmLabel: t('Create new codes'),
      run: async (code) => {
        const r = await api.post<{ recoveryCodes: string[] }>('/me/mfa/recovery-codes', { code })
        onCodes(r.recoveryCodes)
        refresh()
      },
    })
  return (
    <div className="sec-block">
      <div className="sec-block-head">
        <h3><Smartphone size={16} /> {t('Authenticator app')}</h3>
        {status.enabled ? <span className="badge badge-success"><Check size={12} /> {t('On')}</span> : <span className="badge">{t('Off')}</span>}
      </div>
      {status.enabled ? (
        <>
          <p className="small muted">
            {t('On since {date}. Logging in with your password also asks for a code from the app.', { date: shortDate(status.enabledAt) })}{' '}
            {t('{count} recovery codes left.', { count: status.recoveryCodesLeft })}
          </p>
          <div className="sec-actions">
            <button type="button" className="btn" onClick={regenerate}>{t('New recovery codes')}</button>
            <button type="button" className="btn btn-danger" onClick={disable} disabled={lastFactor} title={lastFactor ? t('Required in this workspace: add a passkey first.') : undefined}>{t('Turn off')}</button>
          </div>
          {status.recoveryCodesLeft <= 3 && <p className="small" style={{ color: 'var(--warning)' }}>{t('You’re running out of recovery codes: create new ones.')}</p>}
        </>
      ) : (
        <>
          <p className="small muted">{t('After your password, enter a one-time code from an authenticator app on your phone. Someone who learns your password still can’t sign in.')}</p>
          <button type="button" className="btn btn-primary" onClick={start}><Smartphone size={15} /> {t('Set up an authenticator app')}</button>
        </>
      )}
      {setup && (
        <EnableDialog
          setup={setup}
          onClose={() => setSetup(null)}
          onDone={(codes) => {
            setSetup(null)
            onCodes(codes)
            refresh()
          }}
        />
      )}
    </div>
  )
}

/** Passkeys: sign in with the device's fingerprint, face or PIN, or use one as the second step. */
function PasskeysBlock({ status, ask, lastFactor }: { status: MfaStatus; ask: (a: Ask) => void; lastFactor: boolean }) {
  const qc = useQueryClient()
  const toast = useToast()
  const list = useQuery({ queryKey: ['passkeys'], queryFn: () => api.get<Passkey[]>('/me/passkeys') })
  const [busy, setBusy] = useState(false)
  const [removing, setRemoving] = useState<Passkey | null>(null)
  const changed = () => {
    qc.invalidateQueries({ queryKey: ['passkeys'] })
    qc.invalidateQueries({ queryKey: ['mfa'] })
  }
  const add = async () => {
    setBusy(true)
    try {
      await withPassword(
        async (password) => {
          const o = await api.post<{ token: string; publicKey: Record<string, unknown> }>('/me/passkeys/options', password ? { password } : {})
          const credential = await createPasskey(o.publicKey)
          await api.post('/me/passkeys', { token: o.token, name: device(navigator.userAgent), credential })
          toast(t('Passkey added'))
          changed()
        },
        ask,
        status.hasPassword,
        t('Add a passkey'),
        t('Continue'),
      )
    } catch (e) {
      if (!cancelled(e)) toast(errorText(e))
    } finally {
      setBusy(false)
    }
  }
  const rename = (p: Passkey) =>
    ask({
      title: t('Rename passkey'),
      label: t('Name'),
      kind: 'text',
      initial: p.name,
      confirmLabel: t('Save'),
      run: async (name) => {
        await api.patch(`/me/passkeys/${p.id}`, { name })
        changed()
      },
    })
  const keys = list.data ?? []
  const only = lastFactor && keys.length === 1
  return (
    <div className="sec-block">
      <div className="sec-block-head">
        <h3><Fingerprint size={16} /> {t('Passkeys')}</h3>
        {keys.length > 0 && <span className="badge badge-success">{t('{count} passkeys', { count: keys.length })}</span>}
      </div>
      <p className="small muted">{t('Sign in with your fingerprint, face or device PIN instead of a password, or use a passkey as the second step after it. Passkeys can’t be phished: they only work on this site.')}</p>
      {list.error && <ErrorBox error={list.error} />}
      {keys.length > 0 && (
        <ul className="sec-rows">
          {keys.map((p) => (
            <li key={p.id} className="sec-row">
              <span className="sec-row-icon"><Fingerprint size={16} /></span>
              <div className="sec-row-text">
                <div className="sec-row-name">
                  {p.name}
                  {p.backedUp && <span className="badge" title={t('Synced to your other devices')}>{t('Synced')}</span>}
                </div>
                <div className="faint small">
                  {t('Added {date}', { date: shortDate(p.createdAt) })}
                  {p.lastUsedAt ? ` · ${t('last used {when}', { when: ago(p.lastUsedAt) })}` : ''}
                </div>
              </div>
              <div className="sec-row-actions">
                <button type="button" className="btn btn-sm btn-ghost" onClick={() => rename(p)} aria-label={t('Rename passkey {name}', { name: p.name })}><Pencil size={13} /> {t('Rename')}</button>
                <button
                  type="button"
                  className="btn btn-sm btn-ghost btn-danger"
                  onClick={() => setRemoving(p)}
                  disabled={only}
                  title={only ? t('Required in this workspace: add another passkey or the authenticator app first.') : undefined}
                  aria-label={t('Remove passkey {name}', { name: p.name })}
                >
                  <Trash size={13} /> {t('Remove')}
                </button>
              </div>
            </li>
          ))}
        </ul>
      )}
      {passkeysSupported() ? (
        <button type="button" className={`btn ${keys.length ? '' : 'btn-primary'}`} disabled={busy} onClick={add}><Fingerprint size={15} /> {t('Add a passkey')}</button>
      ) : (
        <p className="faint small" style={{ marginBottom: 0 }}>{t('This browser can’t use passkeys here (they need a secure https connection).')}</p>
      )}
      {removing && (
        <Confirm
          title={t('Remove the passkey “{name}”?', { name: removing.name })}
          body={<p>{t('You can’t sign in with it any more. Remove it from the device or password manager too.')}</p>}
          confirmLabel={t('Remove')}
          danger
          onClose={() => setRemoving(null)}
          onConfirm={async () => {
            const p = removing
            try {
              await withPassword(
                async (password) => {
                  await api.del(`/me/passkeys/${p.id}`, password ? { password } : undefined)
                  toast(t('Passkey removed'))
                  changed()
                },
                ask,
                status.hasPassword,
                t('Remove the passkey “{name}”?', { name: p.name }),
                t('Remove'),
              )
            } catch (e) {
              toast(errorText(e))
            }
          }}
        />
      )}
    </div>
  )
}

/** Where you're signed in. */
function SessionsBlock() {
  const qc = useQueryClient()
  const toast = useToast()
  const sessions = useQuery({ queryKey: ['sessions'], queryFn: () => api.get<Session[]>('/me/sessions') })
  const refresh = () => qc.invalidateQueries({ queryKey: ['sessions'] })
  const list = sessions.data ?? []
  const others = list.filter((s) => !s.current).length
  const signOut = async (s: Session) => {
    try {
      await api.del(`/me/sessions/${s.id}`)
      toast(t('Signed out of {device}', { device: device(s.userAgent) }))
      refresh()
    } catch (e) {
      toast(errorText(e))
    }
  }
  const signOutOthers = async () => {
    try {
      const r = await api.post<{ revoked: number }>('/me/sessions/revoke-others')
      toast(t('Signed out of {count} other sessions', { count: r.revoked }))
      refresh()
    } catch (e) {
      toast(errorText(e))
    }
  }
  return (
    <div className="sec-block">
      <div className="sec-block-head">
        <h3><Monitor size={16} /> {t('Where you’re signed in')}</h3>
        <span className="spacer" />
        {others > 0 && <button type="button" className="btn btn-sm" onClick={signOutOthers}><LogOut size={13} /> {t('Sign out everywhere else')}</button>}
      </div>
      <p className="small muted">{t('Sign out sessions you don’t recognise. API tokens are listed separately below.')}</p>
      {sessions.isLoading && <Loading />}
      {sessions.error && <ErrorBox error={sessions.error} />}
      {list.length > 0 && (
        <ul className="sec-rows">
          {list.map((s) => (
            <li key={s.id} className="sec-row">
              <span className="sec-row-icon"><Monitor size={16} /></span>
              <div className="sec-row-text">
                <div className="sec-row-name">
                  {device(s.userAgent)}
                  {s.current && <span className="badge badge-accent">{t('This browser')}</span>}
                </div>
                <div className="faint small" title={s.userAgent ?? undefined}>
                  {s.ip ? `${s.ip} · ` : ''}
                  <span title={dateTime(s.createdAt)}>{t('signed in {date}', { date: shortDate(s.createdAt) })}</span> · {t('active {when}', { when: ago(s.lastSeenAt) })}
                </div>
              </div>
              {!s.current && (
                <div className="sec-row-actions">
                  <button type="button" className="btn btn-sm" onClick={() => signOut(s)} aria-label={t('Sign out {device}', { device: device(s.userAgent) })}>{t('Sign out')}</button>
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

/** Profile & settings → Security (and the enrollment screen, without the sessions). */
export function SecuritySettings({ gate }: { gate?: boolean }) {
  const mfa = useMfaStatus()
  const { ask, dialog } = useAsk()
  const [codes, setCodes] = useState<string[] | null>(null)
  if (mfa.isLoading) return <Loading />
  if (mfa.error || !mfa.data) return <ErrorBox error={mfa.error ?? t('Not found')} />
  const status = mfa.data
  // the workspace requires a second factor: the last one can't go
  const lastFactor = status.required && status.passkeys + (status.enabled ? 1 : 0) <= 1
  return (
    <>
      <AppBlock status={status} ask={ask} onCodes={setCodes} lastFactor={lastFactor} />
      <PasskeysBlock status={status} ask={ask} lastFactor={lastFactor} />
      {!gate && <SessionsBlock />}
      {dialog}
      {codes && <RecoveryCodes codes={codes} onClose={() => setCodes(null)} />}
    </>
  )
}

// ---- the enrollment screen -------------------------------------------------------------------

/**
 * Whether the enrollment screen shows: while the workspace requires a second factor the person
 * hasn't set up, and until they continue (so the recovery codes stay on screen).
 */
export function useTwoFactorGate(must: boolean) {
  const qc = useQueryClient()
  const [held, setHeld] = useState(false)
  useEffect(() => {
    if (must) setHeld(true)
  }, [must])
  return {
    show: must || held,
    done: async () => {
      await qc.invalidateQueries({ queryKey: ['me'] })
      setHeld(false)
    },
  }
}

/** Shown instead of the app until the person sets up two-factor authentication. */
export function TwoFactorGate({ me, onDone }: { me: Me; onDone: () => void }) {
  const mfa = useMfaStatus()
  const enrolled = !!mfa.data && (mfa.data.enabled || mfa.data.passkeys > 0)
  return (
    <div className="auth">
      <main className="auth-card sec-gate">
        <Logo />
        <h1>{t('Set up two-factor authentication')}</h1>
        <p className="muted">
          {t('{workspace} asks you to protect your account with a second step when you sign in: a code from an authenticator app, or a passkey (your fingerprint, face or device PIN). It takes a minute.', { workspace: me.workspace?.name ?? 'TaskCraft' })}
        </p>
        {enrolled && (
          <div className="sec-done" role="status">
            <ShieldCheck size={18} /> <strong>{t('You’re all set.')}</strong>
            <span className="spacer" />
            <button type="button" className="btn btn-primary" onClick={onDone}>{t('Continue to TaskCraft')}</button>
          </div>
        )}
        <SecuritySettings gate />
        <p className="small faint sec-gate-foot">
          {t('Signed in as {name}.', { name: me.user?.email ?? me.user?.name ?? '' })}{' '}
          <a
            href="/login"
            onClick={async (e) => {
              e.preventDefault()
              await api.post('/auth/logout')
              location.href = '/login'
            }}
          >
            {t('Sign out')}
          </a>
        </p>
      </main>
    </div>
  )
}

// ---- administration --------------------------------------------------------------------------

export type TwoFactorPolicyValue = 'off' | 'admins' | 'everyone'

interface Policy {
  requireTwoFactor: TwoFactorPolicyValue
  /** everyone with a second factor */
  enrolled: string[]
  people: { total: number; enrolled: number }
  admins: { total: number; enrolled: number }
}

export const useTwoFactorPolicy = () => useQuery({ queryKey: ['admin', 'security'], queryFn: () => api.get<Policy>('/admin/security') })

const POLICIES: { id: TwoFactorPolicyValue; label: () => string; desc: () => string }[] = [
  { id: 'off', label: () => t('Optional'), desc: () => t('Everyone decides for themselves.') },
  { id: 'admins', label: () => t('Owners and admins'), desc: () => t('Owners and admins must set up a second factor.') },
  { id: 'everyone', label: () => t('Everyone'), desc: () => t('Everyone must set up a second factor.') },
]

/** Admin → Overview: "Require two-factor authentication". */
export function TwoFactorPolicy() {
  const qc = useQueryClient()
  const toast = useToast()
  const policy = useTwoFactorPolicy()
  const mine = useMfaStatus()
  const [choice, setChoice] = useState<TwoFactorPolicyValue | null>(null)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const current = policy.data?.requireTwoFactor ?? 'off'
  const value = choice ?? current
  const unenrolledMe = !!mine.data && !mine.data.enabled && mine.data.passkeys === 0
  const save = async (e: FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    try {
      await api.put('/admin/security', { requireTwoFactor: value })
      setChoice(null)
      await qc.invalidateQueries({ queryKey: ['admin', 'security'] })
      qc.invalidateQueries({ queryKey: ['mfa'] })
      toast(t('Two-factor policy saved'))
      // without a second factor of your own, the setup comes next
      qc.invalidateQueries({ queryKey: ['me'] })
    } catch (e) {
      setErr(errorText(e))
    } finally {
      setBusy(false)
    }
  }
  const p = policy.data
  return (
    <form className="adm-card" onSubmit={save} style={{ marginTop: 20 }}>
      <div className="adm-card-head">
        <h2>{t('Two-factor authentication')}</h2>
        <p className="small muted">{t('Require a second step at sign-in: a code from an authenticator app or a passkey. People it applies to see only the setup until they’ve added one. API tokens keep working.')}</p>
      </div>
      <div className="adm-card-body">
        {policy.error && <ErrorBox error={policy.error} />}
        <div className="sec-policy" role="radiogroup" aria-label={t('Require two-factor authentication')}>
          {POLICIES.map((o) => (
            <button key={o.id} type="button" role="radio" aria-checked={value === o.id} className={`template ${value === o.id ? 'on' : ''}`} onClick={() => setChoice(o.id)}>
              <strong>{o.label()}</strong>
              <span className="faint small">{o.desc()}</span>
            </button>
          ))}
        </div>
        {p && (
          <div className="sec-figures small muted">
            <span><ShieldCheck size={14} style={{ verticalAlign: '-2px' }} /> {t('{enrolled} of {total} people have a second factor', { enrolled: p.people.enrolled, total: p.people.total })}</span>
            <span>{t('{enrolled} of {total} owners and admins', { enrolled: p.admins.enrolled, total: p.admins.total })}</span>
          </div>
        )}
      </div>
      <div className="adm-card-foot">
        {err && <span className="error-text adm-foot-msg" role="alert">{err}</span>}
        {!err && value !== 'off' && value !== current && unenrolledMe && <span className="small adm-foot-msg" style={{ color: 'var(--warning)' }}>{t('You have no second factor yet: you’ll set one up right after saving.')}</span>}
        <button type="submit" className="btn btn-primary" disabled={busy || value === current}>{t('Save changes')}</button>
      </div>
    </form>
  )
}

/** Admin → Users: whether the admin `me` may reset `user`'s second factor. */
export function canResetTwoFactor(me: Me, user: { id: string; role: string }, enrolled: string[] | undefined) {
  if (!enrolled?.includes(user.id) || user.id === me.user?.id) return false
  return me.user?.role === 'owner' || (user.role !== 'owner' && user.role !== 'admin')
}

/** Admin → Users: reset someone's second factor (lost phone and recovery codes). */
export function ResetTwoFactor({ user, onClose }: { user: { id: string; name: string }; onClose: () => void }) {
  const qc = useQueryClient()
  const toast = useToast()
  return (
    <Confirm
      title={t('Reset two-factor authentication of {name}?', { name: user.name })}
      body={
        <p>
          {t('For when {name} lost their phone and recovery codes: their authenticator app and passkeys are removed, they’re signed out everywhere and their API tokens are deleted. They then sign in with their password and set up a second factor again.', { name: user.name })}
        </p>
      }
      confirmLabel={t('Reset')}
      danger
      onClose={onClose}
      onConfirm={async () => {
        try {
          await api.post(`/users/${user.id}/reset-mfa`)
          toast(t('Two-factor authentication of {name} was reset', { name: user.name }))
          qc.invalidateQueries({ queryKey: ['admin', 'security'] })
        } catch (e) {
          toast(errorText(e))
        }
      }}
    />
  )
}
