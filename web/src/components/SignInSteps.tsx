// Signing in with a second factor: the step after the password (a code or a passkey), and
// signing in with a passkey alone.
import { t } from '../i18n'
import { useEffect, useState, type FormEvent } from 'react'
import { Fingerprint } from 'lucide-react'
import { api } from '../api'
import { cancelled, passkeysSupported, signWithPasskey } from '../passkeys'
import { Logo } from '../ui'
import './Security.css'

const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e))

/** Same-site paths only. */
function go(next?: string | null) {
  location.href = next && /^\/(?![/\\])[^\s\\]*$/.test(next) ? next : '/'
}

/**
 * The second step of a sign-in: a code from the authenticator app (or a recovery code), or a
 * passkey. `token` comes with the login answer; it's empty when a cookie carries the step
 * (`/login?mfa=1` after a password reset or single sign-on). `methods`: null when unknown.
 */
export function SecondStep({ token, methods: known, next, onCancel }: { token: string; methods: string[] | null; next: string | null; onCancel: () => void }) {
  const [methods, setMethods] = useState<string[] | null>(known)
  const [code, setCode] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const mfaToken = token || undefined
  useEffect(() => {
    if (methods !== null) return
    api
      .post<{ methods: string[] }>('/auth/mfa/passkey', { mfaToken })
      .then((r) => setMethods(r.methods))
      .catch((e) => {
        setErr(errorText(e))
        setMethods(['code'])
      })
  }, [methods, mfaToken])
  const submit = async (e: FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    try {
      const r = await api.post<{ next?: string | null }>('/auth/mfa', { mfaToken, code })
      go(r?.next ?? next)
    } catch (e) {
      setErr(errorText(e))
      setBusy(false)
    }
  }
  const usePasskey = async () => {
    setBusy(true)
    setErr('')
    try {
      const step = await api.post<{ publicKey: Record<string, unknown> | null }>('/auth/mfa/passkey', { mfaToken })
      if (!step.publicKey) throw new Error(t('You have no passkey here.'))
      const passkey = await signWithPasskey(step.publicKey)
      const r = await api.post<{ next?: string | null }>('/auth/mfa', { mfaToken, passkey })
      go(r?.next ?? next)
    } catch (e) {
      if (!cancelled(e)) setErr(errorText(e))
      setBusy(false)
    }
  }
  const byCode = methods === null || methods.includes('code')
  const byPasskey = !!methods?.includes('passkey') && passkeysSupported()
  return (
    <div className="auth">
      <form className="auth-card" onSubmit={submit}>
        <Logo />
        <h1>{t('Two-factor sign-in')}</h1>
        {byPasskey && (
          <>
            <p className="muted" style={{ marginTop: 0 }}>{t('Confirm it’s you with your passkey: your fingerprint, face, device PIN or security key.')}</p>
            <button type="button" className="btn btn-primary sec-wide" disabled={busy} onClick={usePasskey} autoFocus={!byCode}>
              <Fingerprint size={16} /> {t('Use your passkey')}
            </button>
            {byCode && <div className="faint small sec-or">{t('or with a code')}</div>}
          </>
        )}
        {byCode && (
          <>
            {!byPasskey && <p className="muted" style={{ marginTop: 0 }}>{t('Enter the 6-digit code from your authenticator app, or one of your recovery codes.')}</p>}
            <div className="field">
              <label htmlFor="mfa-code">{t('Code')}</label>
              <input id="mfa-code" className="input sec-otp" autoComplete="one-time-code" spellCheck={false} autoCapitalize="off" required value={code} onChange={(e) => setCode(e.target.value)} placeholder="123 456" autoFocus={!byPasskey} />
            </div>
          </>
        )}
        {!byCode && !byPasskey && methods !== null && <p className="error-text">{t('This browser can’t use passkeys here. Sign in on a device that can, or ask an admin to reset your second factor.')}</p>}
        {err && <p className="error-text" role="alert">{err}</p>}
        {byCode && <button className={`btn sec-wide ${byPasskey ? '' : 'btn-primary'}`} disabled={busy || !code.trim()}>{t('Verify')}</button>}
        <p className="small" style={{ textAlign: 'center', marginBottom: 0 }}>
          <a
            href="/login"
            onClick={(e) => {
              e.preventDefault()
              history.replaceState(null, '', next ? `/login?next=${encodeURIComponent(next)}` : '/login')
              onCancel()
            }}
          >
            {t('Use a different account')}
          </a>
        </p>
      </form>
    </div>
  )
}

/** "Sign in with a passkey": no password — the passkey verified the person (two factors in one). */
export function PasskeySignIn({ next, onError }: { next: string | null; onError: (message: string) => void }) {
  const [busy, setBusy] = useState(false)
  if (!passkeysSupported()) return null
  const run = async () => {
    setBusy(true)
    onError('')
    try {
      const o = await api.post<{ token: string; publicKey: Record<string, unknown> }>('/auth/passkey/options')
      const credential = await signWithPasskey(o.publicKey)
      await api.post('/auth/passkey', { token: o.token, credential })
      go(next)
    } catch (e) {
      if (!cancelled(e)) onError(errorText(e))
      setBusy(false)
    }
  }
  return (
    <button type="button" className="btn sec-passkey-login" disabled={busy} onClick={run}>
      <Fingerprint size={16} /> {t('Sign in with a passkey')}
    </button>
  )
}
