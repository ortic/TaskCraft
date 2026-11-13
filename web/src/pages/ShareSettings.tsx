// Project settings → Sharing: read-only pages of the project for people without an account (a
// list of issues from a query, or the roadmap), with an optional password and end.
import { t } from '../i18n'
import { useState, type FormEvent } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { ExternalLink, Globe, Lock, Pencil, Plus, Trash } from 'lucide-react'
import { api, ago, shortDate, type Project } from '../api'
import { Confirm, Empty, ErrorBox, Loading, Modal, useToast } from '../ui'
import { CopyField } from './Settings'

interface ShareLink {
  id: string
  kind: 'issues' | 'roadmap'
  title: string
  query: string
  url: string
  people: boolean
  comments: boolean
  password: boolean
  expiresAt: string | null
  expired: boolean
  createdAt: string
  views: number
  lastViewedAt: string | null
}

export default function ShareSettings({ project }: { project: Project }) {
  const qk = ['shares', project.key]
  const links = useQuery({ queryKey: qk, queryFn: () => api.get<ShareLink[]>(`/projects/${project.key}/shares`) })
  const [editing, setEditing] = useState<ShareLink | 'new' | null>(null)
  const [revoking, setRevoking] = useState<ShareLink | null>(null)
  const qc = useQueryClient()
  const toast = useToast()
  return (
    <section className="ps-section">
      <div className="ps-section-head">
        <div className="ps-section-text">
          <h2>{t('Sharing')}</h2>
          <p className="ps-desc">
            {t('Read-only pages for people without an account: a list of issues (a status page) or the roadmap. Private issues and restricted comments are never shown; names only when you choose so.')}
          </p>
        </div>
        <div className="ps-section-actions">
          <button className="btn btn-primary" onClick={() => setEditing('new')}><Plus size={15} /> {t('New link')}</button>
        </div>
      </div>
      {links.isLoading ? (
        <Loading />
      ) : links.error ? (
        <ErrorBox error={links.error} />
      ) : (links.data ?? []).length === 0 ? (
        <Empty icon={<Globe size={30} />} title={t('No links yet')}>{t('Share a status page or the roadmap with customers and other teams.')}</Empty>
      ) : (
        <ul className="ps-list">
          {links.data!.map((l) => (
            <li key={l.id} className="ps-share">
              <div className="ps-share-head">
                <div>
                  <div className="ps-strong">
                    {l.password && <Lock size={13} aria-label={t('With a password')} />} {l.title}
                    <span className="badge">{l.kind === 'roadmap' ? t('Roadmap') : t('Issue list')}</span>
                    {l.expired && <span className="badge badge-warning">{t('Ended')}</span>}
                  </div>
                  <div className="faint small">
                    {[
                      l.kind === 'issues' ? (l.query ? `TQL: ${l.query}` : t('All issues of the project')) : null,
                      l.expiresAt ? t('until {date}', { date: shortDate(l.expiresAt) }) : t('no end'),
                      t('{count} views', { count: l.views }),
                      l.lastViewedAt ? t('last viewed {when}', { when: ago(l.lastViewedAt) }) : null,
                    ].filter(Boolean).join(' · ')}
                  </div>
                </div>
                <div className="row" style={{ gap: 6 }}>
                  <a className="btn btn-sm" href={l.url} target="_blank" rel="noopener noreferrer"><ExternalLink size={14} /> {t('Open')}</a>
                  <button className="btn btn-sm" onClick={() => setEditing(l)}><Pencil size={14} /> {t('Edit')}</button>
                  <button className="btn btn-sm btn-ghost" onClick={() => setRevoking(l)}><Trash size={14} /> {t('Revoke')}</button>
                </div>
              </div>
              <CopyField value={l.url} label={t('Address of {title}', { title: l.title })} />
            </li>
          ))}
        </ul>
      )}
      {editing && <ShareDialog project={project} link={editing === 'new' ? null : editing} onClose={() => setEditing(null)} onSaved={() => qc.invalidateQueries({ queryKey: qk })} />}
      {revoking && (
        <Confirm
          title={t('Revoke {title}?', { title: revoking.title })}
          body={<p>{t('The address stops working at once, for everyone who has it.')}</p>}
          confirmLabel={t('Revoke')}
          danger
          onConfirm={async () => {
            await api.del(`/shares/${revoking.id}`)
            await qc.invalidateQueries({ queryKey: qk })
            toast(t('Link revoked'))
          }}
          onClose={() => setRevoking(null)}
        />
      )}
    </section>
  )
}

const toDay = (iso: string | null) => (iso ? iso.slice(0, 10) : '')

function ShareDialog({ project, link, onClose, onSaved }: { project: Project; link: ShareLink | null; onClose: () => void; onSaved: () => void }) {
  const toast = useToast()
  const [kind, setKind] = useState<'issues' | 'roadmap'>(link?.kind ?? 'issues')
  const [title, setTitle] = useState(link?.title ?? '')
  const [query, setQuery] = useState(link?.query ?? '')
  const [people, setPeople] = useState(link?.people ?? false)
  const [comments, setComments] = useState(link?.comments ?? false)
  const [until, setUntil] = useState(toDay(link?.expiresAt ?? null))
  const [password, setPassword] = useState('')
  const [clearPassword, setClearPassword] = useState(false)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const save = async (e: FormEvent) => {
    e.preventDefault()
    setBusy(true)
    setErr('')
    // the end of the chosen day, where the person is
    const expiresAt = until ? new Date(`${until}T23:59:59`).toISOString() : null
    const body = { kind, title, query: kind === 'issues' ? query : '', people, comments, expiresAt, password: password || undefined, clearPassword }
    try {
      if (link) await api.patch(`/shares/${link.id}`, body)
      else await api.post(`/projects/${project.key}/shares`, body)
      onSaved()
      toast(link ? t('Link saved') : t('Link made: copy its address below'))
      onClose()
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal
      title={link ? t('Edit {title}', { title: link.title }) : t('New link')}
      onClose={onClose}
      width={560}
      footer={
        <>
          {err && <span className="error-text" role="alert" style={{ marginInlineEnd: 'auto' }}>{err}</span>}
          <button type="button" className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button type="submit" form="share-form" className="btn btn-primary" disabled={busy || !title.trim()}>{link ? t('Save') : t('Make link')}</button>
        </>
      }
    >
      <form id="share-form" onSubmit={save} className="ps-share-form">
        {!link && (
          <div className="field">
            <span className="field-label" id="share-kind">{t('What it shows')}</span>
            <div className="seg" role="radiogroup" aria-labelledby="share-kind">
              <button type="button" role="radio" aria-checked={kind === 'issues'} className={kind === 'issues' ? 'active' : ''} onClick={() => setKind('issues')}>{t('Issue list')}</button>
              <button type="button" role="radio" aria-checked={kind === 'roadmap'} className={kind === 'roadmap' ? 'active' : ''} onClick={() => setKind('roadmap')}>{t('Roadmap')}</button>
            </div>
          </div>
        )}
        <div className="field">
          <label htmlFor="share-title">{t('Title')}</label>
          <input id="share-title" className="input" value={title} maxLength={120} onChange={(e) => setTitle(e.target.value)} placeholder={kind === 'roadmap' ? t('Our roadmap') : t('Known issues')} required />
        </div>
        {kind === 'issues' && (
          <div className="field">
            <label htmlFor="share-query">{t('Which issues (TQL)')}</label>
            <input id="share-query" className="input" value={query} onChange={(e) => setQuery(e.target.value)} placeholder="labels = incident and updated > -30d" spellCheck={false} aria-describedby="share-query-hint" />
            <span className="field-hint" id="share-query-hint">{t('Within {project}; empty shows all its issues.', { project: project.name })}</span>
          </div>
        )}
        {kind === 'roadmap' && <p className="faint small">{t('The project’s epics with their dates and progress, and its releases.')}</p>}
        <label className="ps-check">
          <input type="checkbox" checked={people} onChange={(e) => setPeople(e.target.checked)} />
          <span>{t('Show people’s names (assignees, comment authors)')}</span>
        </label>
        <label className="ps-check">
          <input type="checkbox" checked={comments} onChange={(e) => setComments(e.target.checked)} />
          <span>{t('Show comments meant for everyone')}</span>
        </label>
        <div className="ps-grid2">
          <div className="field">
            <label htmlFor="share-until">{t('Works until')}</label>
            <input id="share-until" type="date" className="input" value={until} min={new Date().toISOString().slice(0, 10)} onChange={(e) => setUntil(e.target.value)} aria-describedby="share-until-hint" />
            <span className="field-hint" id="share-until-hint">{t('Empty: until you revoke it.')}</span>
          </div>
          <div className="field">
            <label htmlFor="share-password">{link?.password ? t('New password') : t('Password')}</label>
            <input id="share-password" type="password" className="input" value={password} minLength={8} autoComplete="new-password" disabled={clearPassword} onChange={(e) => setPassword(e.target.value)} aria-describedby="share-password-hint" />
            <span className="field-hint" id="share-password-hint">{link?.password ? t('Leave empty to keep it; a new one locks out who had the old one.') : t('Optional, at least 8 characters.')}</span>
          </div>
        </div>
        {link?.password && (
          <label className="ps-check">
            <input type="checkbox" checked={clearPassword} onChange={(e) => setClearPassword(e.target.checked)} />
            <span>{t('Remove the password')}</span>
          </label>
        )}
      </form>
    </Modal>
  )
}
