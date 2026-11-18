// Administration → Provisioning: SCIM tokens for identity providers, with what each one did last.
import { t } from '../i18n'
import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Copy, KeyRound, Plus, Trash } from 'lucide-react'
import { api, ago, dateTime } from '../api'
import { Confirm, Empty, ErrorBox, Loading, Modal, useToast } from '../ui'

interface ScimToken {
  id: string
  name: string
  providerId: string | null
  defaultRole: 'member' | 'guest'
  createdAt: string
  lastUsedAt: string | null
  requests: number
  lastError: string | null
  lastErrorAt: string | null
  users: number
  deactivated: number
  groups: number
}

interface ScimPage {
  baseUrl: string
  tokens: ScimToken[]
  providers: { id: string; name: string }[]
}

function CopyButton({ text, label }: { text: string; label: string }) {
  const toast = useToast()
  return (
    <button
      type="button"
      className="btn btn-sm"
      aria-label={label}
      onClick={() => void navigator.clipboard?.writeText(text).then(() => toast(t('Copied')), () => toast(t('Copying didn’t work: select the text instead')))}
    >
      <Copy size={13} /> {t('Copy')}
    </button>
  )
}

export default function ProvisioningTab() {
  const qc = useQueryClient()
  const toast = useToast()
  const q = useQuery({ queryKey: ['admin', 'scim'], queryFn: () => api.get<ScimPage>('/admin/scim') })
  const [adding, setAdding] = useState(false)
  const [made, setMade] = useState<{ name: string; token: string } | null>(null)
  const [deleting, setDeleting] = useState<ScimToken | null>(null)
  const refresh = () => qc.invalidateQueries({ queryKey: ['admin', 'scim'] })
  const list = q.data?.tokens ?? []
  return (
    <>
      <div className="adm-toolbar">
        <p className="small muted adm-intro">
          {t('With SCIM, your identity provider (Microsoft Entra ID, Okta, …) creates accounts before people sign in, keeps names and teams in step, and deactivates people who leave — which ends their sessions and revokes their API tokens at once. Nobody is deleted: their history stays.')}
        </p>
        <span className="spacer" />
        <button className="btn btn-primary" onClick={() => setAdding(true)}><Plus size={15} /> {t('Add a token')}</button>
      </div>
      {q.isLoading && <Loading />}
      {q.error && <ErrorBox error={q.error} />}
      {q.data && (
        <div className="adm-card" style={{ marginBottom: 14 }}>
          <div className="adm-card-body">
            <div className="field" style={{ marginBottom: 10 }}>
              <label htmlFor="scim-url">{t('SCIM address (tenant URL) for the identity provider')}</label>
              <div className="row" style={{ gap: 8 }}>
                <input id="scim-url" className="input" readOnly value={q.data.baseUrl} onFocus={(e) => e.currentTarget.select()} />
                <CopyButton text={q.data.baseUrl} label={t('Copy the SCIM address')} />
              </div>
            </div>
          </div>
        </div>
      )}
      {q.data && list.length === 0 && (
        <Empty icon={<KeyRound size={32} />} title={t('No provisioning tokens yet')}>{t('Make one per identity provider and enter it there as the secret (bearer) token.')}</Empty>
      )}
      <div className="adm-hooks">
        {list.map((k) => (
          <div key={k.id} className="adm-hook">
            <div className="adm-hook-head">
              <KeyRound size={15} className="faint" aria-hidden />
              <strong className="adm-hook-url">{k.name}</strong>
              {k.providerId && <span className="badge badge-accent" title={t('Shares teams with the group sync of this single sign-on provider')}>{q.data?.providers.find((p) => p.id === k.providerId)?.name ?? k.providerId}</span>}
              {k.defaultRole === 'guest' && <span className="badge">{t('New people are guests')}</span>}
            </div>
            <div className="adm-hook-events">
              <span className="label-chip">{t('{count} people', { count: k.users })}</span>
              {k.deactivated > 0 && <span className="label-chip">{t('{count} deactivated', { count: k.deactivated })}</span>}
              <span className="label-chip">{t('{count} teams', { count: k.groups })}</span>
              <span className="label-chip">{t('{count} requests', { count: k.requests })}</span>
            </div>
            <div className="adm-hook-foot">
              <span className={`small ${k.lastUsedAt ? 'adm-good' : 'faint'}`} title={k.lastUsedAt ? dateTime(k.lastUsedAt) : undefined}>
                {k.lastUsedAt ? t('Last request {when}', { when: ago(k.lastUsedAt) }) : t('Not used yet')}
              </span>
              {k.lastError && (
                <span className="small adm-bad" title={k.lastErrorAt ? dateTime(k.lastErrorAt) : undefined}>
                  {t('Last error {when}: {error}', { when: ago(k.lastErrorAt), error: k.lastError })}
                </span>
              )}
              <span className="spacer" />
              <button className="icon-btn icon-btn-sm" title={t('Delete token')} aria-label={t('Delete the token {name}', { name: k.name })} onClick={() => setDeleting(k)}><Trash size={14} /></button>
            </div>
          </div>
        ))}
      </div>
      {adding && q.data && (
        <AddToken
          providers={q.data.providers}
          onClose={() => setAdding(false)}
          onCreated={(name, token) => {
            setAdding(false)
            setMade({ name, token })
            refresh()
          }}
        />
      )}
      {made && (
        <Modal title={t('Token for {name}', { name: made.name })} onClose={() => setMade(null)} width={560} footer={<button className="btn btn-primary" onClick={() => setMade(null)}>{t('Done')}</button>}>
          <p style={{ marginTop: 0 }}>{t('Copy the token now and enter it in your identity provider as the secret token. It isn’t shown again.')}</p>
          <div className="row" style={{ gap: 8 }}>
            <input className="input" readOnly value={made.token} aria-label={t('The token')} onFocus={(e) => e.currentTarget.select()} style={{ fontFamily: 'var(--font-mono, monospace)' }} />
            <CopyButton text={made.token} label={t('Copy the token')} />
          </div>
        </Modal>
      )}
      {deleting && (
        <Confirm
          title={t('Delete the token {name}?', { name: deleting.name })}
          body={<p>{t('The identity provider can’t provision any more. The people and teams it made stay as they are.')}</p>}
          confirmLabel={t('Delete token')}
          danger
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            try {
              await api.del(`/admin/scim/${deleting.id}`)
              refresh()
            } catch (e) {
              toast((e as Error).message)
            }
            setDeleting(null)
          }}
        />
      )}
    </>
  )
}

function AddToken({ providers, onClose, onCreated }: { providers: ScimPage['providers']; onClose: () => void; onCreated: (name: string, token: string) => void }) {
  const [name, setName] = useState('')
  const [provider, setProvider] = useState('')
  const [role, setRole] = useState<'member' | 'guest'>('member')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const save = async () => {
    setBusy(true)
    setErr('')
    try {
      const r = await api.post<{ token: string }>('/admin/scim', { name: name.trim(), providerId: provider || undefined, defaultRole: role })
      onCreated(name.trim(), r.token)
    } catch (e) {
      setErr((e as Error).message)
      setBusy(false)
    }
  }
  return (
    <Modal
      title={t('Add a provisioning token')}
      onClose={onClose}
      width={520}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !name.trim()} onClick={save}>{t('Create token')}</button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="scim-name">{t('Name')}</label>
        <input id="scim-name" className="input" autoFocus maxLength={80} value={name} placeholder={t('e.g. Entra ID')} onChange={(e) => setName(e.target.value)} />
      </div>
      {providers.length > 0 && (
        <div className="field">
          <label htmlFor="scim-provider">{t('Single sign-on provider')}</label>
          <select id="scim-provider" className="select" value={provider} onChange={(e) => setProvider(e.target.value)}>
            <option value="">{t('None')}</option>
            {providers.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
          </select>
          <span className="field-hint">{t('Its groups are then the same teams the group sync at sign-in manages.')}</span>
        </div>
      )}
      <div className="field">
        <label htmlFor="scim-role">{t('Role of the people it creates')}</label>
        <select id="scim-role" className="select" value={role} onChange={(e) => setRole(e.target.value as 'member' | 'guest')}>
          <option value="member">{t('Member')}</option>
          <option value="guest">{t('Guest')}</option>
        </select>
      </div>
      {err && <p className="error-text" role="alert">{err}</p>}
    </Modal>
  )
}
