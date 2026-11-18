// /cli/authorize?code=ABCD-EFGH: the page `tc login` opens. The signed-in person sees which
// computer asks to act as them and approves or refuses; nothing happens by opening the page.
import { t } from '../i18n'
import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Terminal } from 'lucide-react'
import { api, ago } from '../api'
import { Loading, Logo, useMe } from '../ui'

interface CliLogin {
  name: string
  createdAt: string
  approved: boolean
}

export default function CliAuthorize() {
  const me = useMe().data
  const code = (new URLSearchParams(location.search).get('code') ?? '').trim().toUpperCase()
  const path = `/cli/logins/${encodeURIComponent(code)}`
  const q = useQuery({ queryKey: ['cli-login', code], queryFn: () => api.get<CliLogin>(path), retry: false, enabled: !!code })
  const [state, setState] = useState<'' | 'approved' | 'denied'>('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const act = async (approve: boolean) => {
    setBusy(true)
    setErr('')
    try {
      if (approve) await api.post(`${path}/approve`)
      else await api.del(path)
      setState(approve ? 'approved' : 'denied')
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  const gone = !code || !!q.error
  return (
    <div className="auth">
      <main className="auth-card">
        <Logo />
        {q.isLoading && <Loading />}
        {state === 'approved' || (q.data?.approved && !state) ? (
          <>
            <h1>{t('The command line is signed in')}</h1>
            <p className="muted">{t('Go back to your terminal. You can revoke the token any time under Profile & settings → API tokens.')}</p>
          </>
        ) : state === 'denied' ? (
          <>
            <h1>{t('Sign-in refused')}</h1>
            <p className="muted">{t('Nothing was signed in.')}</p>
          </>
        ) : gone ? (
          <>
            <h1>{t('This sign-in doesn’t exist any more')}</h1>
            <p className="muted">{t('Codes work for ten minutes. Run `tc login` again.')}</p>
          </>
        ) : q.data ? (
          <>
            <h1><Terminal size={20} aria-hidden /> {t('Sign in the command line?')}</h1>
            <p>{t('The command-line client on {name} asks to act as you, {user}.', { name: q.data.name, user: me?.user?.name ?? '' })}</p>
            <p className="muted small">
              {t('Check that the code matches the one in your terminal:')} <strong className="issue-key" style={{ fontSize: 15 }}>{code}</strong> · {ago(q.data.createdAt)}
            </p>
            <p className="muted small">{t('It gets an API token of its own with your permissions. If you didn’t run `tc login` just now, refuse.')}</p>
            {err && <p className="error-text" role="alert">{err}</p>}
            <div className="row" style={{ gap: 8 }}>
              <button className="btn btn-primary" style={{ flex: 1 }} disabled={busy} onClick={() => act(true)}>{t('Approve')}</button>
              <button className="btn" style={{ flex: 1 }} disabled={busy} onClick={() => act(false)}>{t('Refuse')}</button>
            </div>
          </>
        ) : null}
        <p className="small" style={{ marginBottom: 0, marginTop: 16 }}><a href="/">{t('Open TaskCraft')}</a></p>
      </main>
    </div>
  )
}
