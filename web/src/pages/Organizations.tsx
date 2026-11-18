import { t } from '../i18n'
import { useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Building2, Plus, Trash, Upload } from 'lucide-react'
import { api, type Issue } from '../api'
import { Confirm, Empty, ErrorBox, Loading, Lozenge, useMe, useToast } from '../ui'
import { useOpenIssue } from '../components/Layout'

export interface Organization {
  id: string
  name: string
  domains: string[]
  slaPercent: number
  memberCount: number
  openRequests: number
}

interface OrganizationDetail {
  id: string
  name: string
  domains: string[]
  notes: string | null
  slaPercent: number
  members: { email: string; name: string | null }[]
  requests: Issue[]
}

export function useOrganizations(enabled = true) {
  return useQuery({ queryKey: ['organizations'], queryFn: () => api.get<Organization[]>('/organizations'), staleTime: 60_000, enabled })
}

export default function Organizations() {
  const { id } = useParams()
  return id ? <OrganizationPage id={id} /> : <OrganizationList />
}

function OrganizationList() {
  const qc = useQueryClient()
  const toast = useToast()
  const nav = useNavigate()
  const list = useOrganizations()
  const [name, setName] = useState('')
  const [domains, setDomains] = useState('')
  const create = async () => {
    try {
      const r = await api.post<{ id: string }>('/organizations', { name: name.trim(), domains })
      qc.invalidateQueries({ queryKey: ['organizations'] })
      nav(`/organizations/${r.id}`)
    } catch (e) {
      toast((e as Error).message)
    }
  }
  const importCsv = async (file: File) => {
    try {
      const r = await api.postFile<{ organizations: number; members: number; skipped: number }>('/organizations/import', file)
      toast(t('Organizations added: {orgs}; members added: {members}', { orgs: r.organizations, members: r.members }))
      qc.invalidateQueries({ queryKey: ['organizations'] })
    } catch (e) {
      toast((e as Error).message)
    }
  }
  return (
    <div className="page">
      <div className="page-head">
        <h1><Building2 size={20} /> {t('Organizations')}</h1>
      </div>
      <p className="muted small" style={{ maxWidth: 720 }}>
        {t('The customers whose people write to the service desk. Requesters from an organization’s domains join it by themselves; their requests show the organization, and in the portal they can share requests with colleagues.')}
      </p>
      <div className="row-wrap" style={{ gap: 8, margin: '12px 0' }}>
        <input className="input input-sm" value={name} onChange={(e) => setName(e.target.value)} placeholder={t('Name, like Acme Ltd')} aria-label={t('Organization name')} />
        <input className="input input-sm" value={domains} onChange={(e) => setDomains(e.target.value)} placeholder={t('Mail domains, like acme.com')} aria-label={t('Mail domains')} />
        <button className="btn btn-sm btn-primary" disabled={!name.trim()} onClick={create}><Plus size={14} /> {t('Add organization')}</button>
        <label className="btn btn-sm" title={t('A CSV with the columns organization, email and name')}>
          <Upload size={14} /> {t('Import CSV')}
          <input type="file" accept=".csv,text/csv" hidden onChange={(e) => e.target.files?.[0] && importCsv(e.target.files[0])} />
        </label>
      </div>
      {list.isLoading && <Loading />}
      {list.error && <ErrorBox error={list.error} />}
      {list.data && list.data.length === 0 && <Empty icon={<Building2 size={32} />} title={t('No organizations yet')} />}
      {(list.data?.length ?? 0) > 0 && (
        <table className="list">
          <thead>
            <tr><th>{t('Organization')}</th><th>{t('Mail domains')}</th><th>{t('Members')}</th><th>{t('Open requests')}</th></tr>
          </thead>
          <tbody>
            {list.data!.map((o) => (
              <tr key={o.id}>
                <td><Link to={`/organizations/${o.id}`}>{o.name}</Link></td>
                <td className="small faint">{o.domains.join(', ')}</td>
                <td className="small">{o.memberCount}</td>
                <td className="small">{o.openRequests}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  )
}

function OrganizationPage({ id }: { id: string }) {
  const qc = useQueryClient()
  const toast = useToast()
  const nav = useNavigate()
  const openIssue = useOpenIssue()
  const isAdmin = ['owner', 'admin'].includes(useMe().data?.user?.role ?? '')
  const q = useQuery({ queryKey: ['organization', id], queryFn: () => api.get<OrganizationDetail>(`/organizations/${id}`) })
  const [email, setEmail] = useState('')
  const [memberName, setMemberName] = useState('')
  const [deleting, setDeleting] = useState(false)
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['organization', id] })
    qc.invalidateQueries({ queryKey: ['organizations'] })
  }
  const run = async (fn: () => Promise<unknown>) => {
    try {
      await fn()
    } catch (e) {
      toast((e as Error).message)
    } finally {
      refresh()
    }
  }
  if (q.isLoading) return <Loading />
  if (q.error) return <div className="page"><ErrorBox error={q.error} /></div>
  const o = q.data!
  return (
    <div className="page">
      <div className="crumbs"><Link to="/organizations">{t('Organizations')}</Link></div>
      <div className="page-head">
        <h1><Building2 size={20} /> {o.name}</h1>
        <span className="spacer" />
        {isAdmin && <button className="btn btn-sm btn-danger" onClick={() => setDeleting(true)}><Trash size={14} /> {t('Delete')}</button>}
      </div>
      <div className="row-wrap" style={{ gap: 16, alignItems: 'flex-start' }}>
        <div className="field" style={{ minWidth: 220 }}>
          <label htmlFor="org-name">{t('Name')}</label>
          <input id="org-name" className="input" defaultValue={o.name} onBlur={(e) => e.target.value.trim() !== o.name && run(() => api.patch(`/organizations/${id}`, { name: e.target.value.trim() }))} />
        </div>
        <div className="field" style={{ minWidth: 220 }}>
          <label htmlFor="org-domains">{t('Mail domains')}</label>
          <input id="org-domains" className="input" defaultValue={o.domains.join(', ')} onBlur={(e) => e.target.value !== o.domains.join(', ') && run(() => api.patch(`/organizations/${id}`, { domains: e.target.value }))} />
          <span className="field-hint">{t('Requesters writing from these join by themselves.')}</span>
        </div>
        <div className="field">
          <label htmlFor="org-sla">{t('Service-level goals')}</label>
          <select id="org-sla" className="select" value={o.slaPercent} onChange={(e) => run(() => api.patch(`/organizations/${id}`, { slaPercent: Number(e.target.value) }))}>
            {[25, 50, 75, 100, 150, 200].map((p) => <option key={p} value={p}>{p === 100 ? t('As the project says') : t('{pct}% of the project’s', { pct: p })}</option>)}
          </select>
        </div>
      </div>
      <div className="field">
        <label htmlFor="org-notes">{t('Notes for the team')}</label>
        <textarea id="org-notes" className="input" rows={3} defaultValue={o.notes ?? ''} onBlur={(e) => e.target.value !== (o.notes ?? '') && run(() => api.patch(`/organizations/${id}`, { notes: e.target.value }))} />
      </div>

      <h2 className="small-head">{t('Members')}</h2>
      <div className="row-wrap" style={{ gap: 8, marginBottom: 8 }}>
        <input className="input input-sm" type="email" value={email} onChange={(e) => setEmail(e.target.value)} placeholder={t('E-mail address')} aria-label={t('E-mail address')} />
        <input className="input input-sm" value={memberName} onChange={(e) => setMemberName(e.target.value)} placeholder={t('Name (optional)')} aria-label={t('Name')} />
        <button className="btn btn-sm" disabled={!email.includes('@')} onClick={() => run(async () => { await api.post(`/organizations/${id}/members`, { email, name: memberName }); setEmail(''); setMemberName('') })}><Plus size={14} /> {t('Add member')}</button>
      </div>
      {o.members.length === 0 ? <p className="faint small">{t('No members yet.')}</p> : (
        <ul className="plain-list">
          {o.members.map((m) => (
            <li key={m.email} className="row" style={{ gap: 8 }}>
              {m.name && <span>{m.name}</span>}
              <span className="faint small">{m.email}</span>
              <button className="icon-btn icon-btn-sm" aria-label={t('Remove {name}', { name: m.email })} title={t('Remove')} onClick={() => run(() => api.del(`/organizations/${id}/members/${encodeURIComponent(m.email)}`))}><Trash size={13} /></button>
            </li>
          ))}
        </ul>
      )}

      <h2 className="small-head">{t('Requests')}</h2>
      {o.requests.length === 0 ? <p className="faint small">{t('No requests yet.')}</p> : (
        <div className="col" style={{ gap: 2 }}>
          {o.requests.map((i) => (
            <button key={i.id} className="menu-item" onClick={() => openIssue(i.key)}>
              <span className="issue-key">{i.key}</span>
              <span style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', textAlign: 'start' }}>{i.title}</span>
              <Lozenge name={i.status.name} category={i.status.category} />
            </button>
          ))}
        </div>
      )}
      {deleting && (
        <Confirm
          title={t('Delete {name}?', { name: o.name })}
          body={<p>{t('Its requests keep their requesters; they just belong to no organization any more.')}</p>}
          confirmLabel={t('Delete')}
          danger
          onConfirm={async () => { await api.del(`/organizations/${id}`); qc.invalidateQueries({ queryKey: ['organizations'] }); nav('/organizations') }}
          onClose={() => setDeleting(false)}
        />
      )}
    </div>
  )
}
