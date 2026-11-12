import { t } from '../i18n'
import { useState } from 'react'
import { Link, useLocation } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { api } from '../api'
import { Logo, useSetup } from '../ui'

function go(next?: string | null) {
  location.href = next && next.startsWith('/') && !next.startsWith('//') ? next : '/'
}

export function Setup() {
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  return (
    <div className="auth">
      <form
        className="auth-card"
        onSubmit={async (e) => {
          e.preventDefault()
          const fd = new FormData(e.currentTarget)
          setBusy(true)
          setErr('')
          try {
            await api.post('/setup', {
              workspaceName: fd.get('workspace'),
              name: fd.get('name'),
              email: fd.get('email'),
              password: fd.get('password'),
              demo: fd.get('demo') === 'on',
            })
            go('/')
          } catch (e) {
            setErr((e as Error).message)
            setBusy(false)
          }
        }}
      >
        <Logo />
        <h1>{t('Welcome to TaskCraft')}</h1>
        <p className="muted" style={{ marginTop: 0 }}>{t('Set up your workspace. You become its owner.')}</p>
        <div className="field"><label htmlFor="ws">{t('Workspace name')}</label><input id="ws" name="workspace" className="input" required placeholder={t('Acme Inc.')} /></div>
        <div className="field"><label htmlFor="nm">{t('Your name')}</label><input id="nm" name="name" className="input" required autoComplete="name" /></div>
        <div className="field"><label htmlFor="em">{t('E-mail')}</label><input id="em" name="email" type="email" className="input" required autoComplete="email" /></div>
        <div className="field"><label htmlFor="pw">{t('Password')}</label><input id="pw" name="password" type="password" className="input" required minLength={8} autoComplete="new-password" /><span className="field-hint">{t('At least 8 characters')}</span></div>
        <label className="row small" style={{ marginBottom: 14 }}><input type="checkbox" name="demo" defaultChecked /> {t('Add sample projects to explore (you can delete them later)')}</label>
        {err && <p className="error-text">{err}</p>}
        <button className="btn btn-primary" style={{ width: '100%' }} disabled={busy}>{busy ? t('Setting up…') : t('Create workspace')}</button>
      </form>
    </div>
  )
}

export function Login() {
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const [email, setEmail] = useState('')
  const mail = !!useSetup().data?.mail
  const next = new URLSearchParams(location.search).get('next')
  return (
    <div className="auth">
      <form
        className="auth-card"
        onSubmit={async (e) => {
          e.preventDefault()
          const fd = new FormData(e.currentTarget)
          setBusy(true)
          setErr('')
          try {
            await api.post('/auth/login', { email: fd.get('email'), password: fd.get('password') })
            go(next)
          } catch (e) {
            setErr((e as Error).message)
            setBusy(false)
          }
        }}
      >
        <Logo />
        <h1>{t('Log in')}</h1>
        <div className="field"><label htmlFor="em">{t('E-mail')}</label><input id="em" name="email" type="email" className="input" required autoFocus autoComplete="username" value={email} onChange={(e) => setEmail(e.target.value)} /></div>
        <div className="field"><label htmlFor="pw">{t('Password')}</label><input id="pw" name="password" type="password" className="input" required autoComplete="current-password" /></div>
        {err && <p className="error-text">{err}</p>}
        <button className="btn btn-primary" style={{ width: '100%' }} disabled={busy}>{t('Log in')}</button>
        {mail ? (
          <p className="small" style={{ marginBottom: 0 }}><Link to="/forgot-password" state={{ email }}>{t('Forgot password?')}</Link></p>
        ) : (
          <p className="faint small" style={{ marginBottom: 0 }}>{t('Forgot your password? Ask an administrator for a reset link.')}</p>
        )}
      </form>
    </div>
  )
}

/** `/forgot-password`: e-mails a link to choose a new password (when the server sends e-mail). */
export function ForgotPassword() {
  const loc = useLocation()
  const [email, setEmail] = useState<string>((loc.state as { email?: string } | null)?.email ?? '')
  const [sent, setSent] = useState(false)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  return (
    <div className="auth">
      <form
        className="auth-card"
        onSubmit={async (e) => {
          e.preventDefault()
          setBusy(true)
          setErr('')
          try {
            await api.post('/auth/forgot', { email: email.trim() })
            setSent(true)
          } catch (e) {
            setErr((e as Error).message)
          } finally {
            setBusy(false)
          }
        }}
      >
        <Logo />
        <h1>{t('Reset your password')}</h1>
        {sent ? (
          <div role="status">
            <p className="muted" style={{ marginTop: 0 }}>{t('If {email} belongs to an account, we’ve sent it a link to choose a new password. The link works once, for an hour.', { email: email.trim() })}</p>
            <p className="faint small">{t('Nothing arrived? Check your spam folder, or ask an administrator for a reset link.')}</p>
          </div>
        ) : (
          <>
            <p className="muted" style={{ marginTop: 0 }}>{t('Enter the e-mail address of your account and we’ll send you a link to choose a new password.')}</p>
            <div className="field"><label htmlFor="fp-em">{t('E-mail')}</label><input id="fp-em" name="email" type="email" className="input" required autoFocus autoComplete="username" value={email} onChange={(e) => setEmail(e.target.value)} /></div>
            {err && <p className="error-text" role="alert">{err}</p>}
            <button className="btn btn-primary" style={{ width: '100%' }} disabled={busy}>{busy ? t('Sending…') : t('Send reset link')}</button>
          </>
        )}
        <p className="small" style={{ marginBottom: 0 }}><Link to="/login">{t('Back to log in')}</Link></p>
      </form>
    </div>
  )
}

/** Invitation and password-reset links. */
export function Welcome() {
  const token = new URLSearchParams(location.search).get('token') ?? ''
  const info = useQuery({ queryKey: ['link', token], queryFn: () => api.get<{ kind: 'invite' | 'reset'; name: string; email: string; workspace: string }>(`/auth/link?token=${encodeURIComponent(token)}`), retry: false })
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  return (
    <div className="auth">
      <form
        className="auth-card"
        onSubmit={async (e) => {
          e.preventDefault()
          const fd = new FormData(e.currentTarget)
          setBusy(true)
          setErr('')
          try {
            await api.post('/auth/link', { token, password: fd.get('password'), name: fd.get('name') })
            go('/')
          } catch (e) {
            setErr((e as Error).message)
            setBusy(false)
          }
        }}
      >
        <Logo />
        {info.error ? (
          <>
            <h1>{t('This link doesn’t work')}</h1>
            <p className="muted">{(info.error as Error).message}</p>
            <a className="btn" href="/login">{t('Go to log in')}</a>
          </>
        ) : info.data ? (
          <>
            <h1>{info.data.kind === 'invite' ? t('Join {workspace}', { workspace: info.data.workspace }) : t('Choose a new password')}</h1>
            <p className="muted" style={{ marginTop: 0 }}>{info.data.email}</p>
            {info.data.kind === 'invite' && <div className="field"><label htmlFor="nm">{t('Your name')}</label><input id="nm" name="name" className="input" defaultValue={info.data.name} /></div>}
            <div className="field"><label htmlFor="pw">{t('Password')}</label><input id="pw" name="password" type="password" className="input" required minLength={8} autoFocus autoComplete="new-password" /></div>
            {err && <p className="error-text">{err}</p>}
            <button className="btn btn-primary" style={{ width: '100%' }} disabled={busy}>{info.data.kind === 'invite' ? t('Join') : t('Save password')}</button>
          </>
        ) : null}
      </form>
    </div>
  )
}
