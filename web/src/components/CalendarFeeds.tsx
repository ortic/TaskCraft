import { t } from '../i18n'
import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { CalendarPlus, Check, Download, RefreshCw, Trash } from 'lucide-react'
import { api, ago, shortDate, type SavedView } from '../api'
import { Confirm, Modal, useFeature, useProjects, useToast } from '../ui'
import { CopyField } from '../pages/Settings'

export interface CalendarFeed {
  id: string
  kind: 'mine' | 'filter' | 'project'
  viewId: string | null
  viewName: string | null
  projectKey: string | null
  parts: { issues?: boolean; sprints?: boolean; releases?: boolean }
  createdAt: string
  lastUsedAt: string | null
}

function feedName(f: CalendarFeed) {
  if (f.kind === 'filter') return t('Filter “{name}”', { name: f.viewName ?? '' })
  if (f.kind === 'project') return t('Project {key}', { key: f.projectKey ?? '' })
  return t('My work')
}

/** The address of a new or renewed feed: shown once, with how to use it. */
function FeedAddress({ url, onDone }: { url: string; onDone: () => void }) {
  return (
    <div className="st-reveal" role="status">
      <div className="st-reveal-head">
        <Check size={16} /> <strong>{t('Your calendar address')}</strong>
        <span className="spacer" />
        <button type="button" className="btn btn-sm btn-ghost" onClick={onDone}>{t('Done [[close]]')}</button>
      </div>
      <p className="small">{t('Add it to your calendar app as a subscription (“from URL”). Copy it now: it’s shown only once. Anyone who has it sees what the feed shows.')}</p>
      <CopyField value={url} label={t('Calendar address')} />
    </div>
  )
}

/** Profile & settings: the person's calendar feeds. */
export function CalendarFeedsSettings() {
  const qc = useQueryClient()
  const toast = useToast()
  const on = useFeature('calendarFeeds')
  const feeds = useQuery({ queryKey: ['calendar-feeds'], queryFn: () => api.get<CalendarFeed[]>('/me/calendar-feeds'), enabled: on })
  const views = useQuery({ queryKey: ['views'], queryFn: () => api.get<SavedView[]>('/views'), enabled: on })
  const projects = useProjects().data ?? []
  const [kind, setKind] = useState<'mine' | 'filter' | 'project'>('mine')
  const [target, setTarget] = useState('')
  const [sprints, setSprints] = useState(true)
  const [releases, setReleases] = useState(true)
  const [url, setUrl] = useState<string | null>(null)
  const [deleting, setDeleting] = useState<CalendarFeed | null>(null)
  if (!on) return <p className="faint small">{t('Calendar feeds are turned off in this workspace.')}</p>
  const create = async () => {
    try {
      const body: Record<string, unknown> = { kind }
      if (kind === 'filter') body.viewId = target
      if (kind === 'project') body.projectKey = target
      if (kind !== 'filter') body.parts = { sprints, releases }
      const r = await api.post<{ url: string }>('/me/calendar-feeds', body)
      setUrl(r.url)
    } catch (e) {
      toast((e as Error).message)
    } finally {
      qc.invalidateQueries({ queryKey: ['calendar-feeds'] })
    }
  }
  const renew = async (f: CalendarFeed) => {
    try {
      const r = await api.post<{ url: string }>(`/me/calendar-feeds/${f.id}/renew`, {})
      setUrl(r.url)
    } catch (e) {
      toast((e as Error).message)
    } finally {
      qc.invalidateQueries({ queryKey: ['calendar-feeds'] })
    }
  }
  const list = feeds.data ?? []
  return (
    <>
      <div className="row-wrap" style={{ gap: 8, alignItems: 'flex-end' }}>
        <div className="field" style={{ marginBottom: 0 }}>
          <label htmlFor="cf-kind">{t('What it shows')}</label>
          <select id="cf-kind" className="select" value={kind} onChange={(e) => { setKind(e.target.value as typeof kind); setTarget('') }}>
            <option value="mine">{t('My work: my issues with due dates, sprints and releases of my projects')}</option>
            <option value="filter">{t('A saved filter’s issues with due dates')}</option>
            <option value="project">{t('A project: issues with due dates, sprints and releases')}</option>
          </select>
        </div>
        {kind === 'filter' && (
          <select className="select" value={target} onChange={(e) => setTarget(e.target.value)} aria-label={t('Saved filter')}>
            <option value="">{t('Choose a filter…')}</option>
            {(views.data ?? []).map((v) => <option key={v.id} value={v.id}>{v.name}</option>)}
          </select>
        )}
        {kind === 'project' && (
          <select className="select" value={target} onChange={(e) => setTarget(e.target.value)} aria-label={t('Project')}>
            <option value="">{t('Choose a project…')}</option>
            {projects.map((p) => <option key={p.id} value={p.key}>{p.name}</option>)}
          </select>
        )}
        {kind !== 'filter' && (
          <>
            <label className="row small" style={{ gap: 6 }}><input type="checkbox" checked={sprints} onChange={(e) => setSprints(e.target.checked)} /> {t('Sprints')}</label>
            <label className="row small" style={{ gap: 6 }}><input type="checkbox" checked={releases} onChange={(e) => setReleases(e.target.checked)} /> {t('Releases')}</label>
          </>
        )}
        <button type="button" className="btn btn-primary" disabled={kind !== 'mine' && !target} onClick={create}><CalendarPlus size={15} /> {t('Create feed')}</button>
      </div>
      {url && <FeedAddress url={url} onDone={() => setUrl(null)} />}
      {feeds.data && list.length === 0 && <p className="faint small st-none">{t('You have no calendar feeds yet.')}</p>}
      {list.length > 0 && (
        <div className="st-table-wrap">
          <table className="list st-tokens">
            <thead>
              <tr>
                <th>{t('Feed')}</th>
                <th>{t('Created')}</th>
                <th>{t('Last read')}</th>
                <th><span className="sr-only">{t('Actions')}</span></th>
              </tr>
            </thead>
            <tbody>
              {list.map((f) => (
                <tr key={f.id}>
                  <td>{feedName(f)}{f.parts.sprints === false || f.parts.releases === false ? <span className="faint small"> · {[f.parts.sprints !== false && t('Sprints'), f.parts.releases !== false && t('Releases')].filter(Boolean).join(', ') || t('issues only')}</span> : null}</td>
                  <td className="small">{shortDate(f.createdAt)}</td>
                  <td className="small">{f.lastUsedAt ? ago(f.lastUsedAt) : <span className="faint">{t('Never')}</span>}</td>
                  <td className="st-tok-actions">
                    <button type="button" className="btn btn-sm" title={t('A new address; the old one stops working')} onClick={() => renew(f)}><RefreshCw size={14} /> {t('New address')}</button>
                    <button type="button" className="btn btn-sm btn-danger" onClick={() => setDeleting(f)}><Trash size={14} /> {t('Delete')}</button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {deleting && (
        <Confirm
          title={t('Delete the feed “{name}”?', { name: feedName(deleting) })}
          body={<p>{t('Calendars subscribed to it stop updating.')}</p>}
          confirmLabel={t('Delete')}
          danger
          onConfirm={async () => {
            await api.del(`/me/calendar-feeds/${deleting.id}`)
            qc.invalidateQueries({ queryKey: ['calendar-feeds'] })
          }}
          onClose={() => setDeleting(null)}
        />
      )}
    </>
  )
}

/** On a project's calendar: download its due dates, or subscribe to the project's feed. */
export function CalendarExport({ projectKey }: { projectKey: string }) {
  const on = useFeature('calendarFeeds')
  const toast = useToast()
  const qc = useQueryClient()
  const [url, setUrl] = useState<string | null>(null)
  return (
    <>
      <a className="btn btn-sm" href={`/api/search.ics?project=${encodeURIComponent(projectKey)}&q=${encodeURIComponent('due is not empty')}`} download>
        <Download size={14} /> {t('Download .ics')}
      </a>
      {on && (
        <button
          type="button"
          className="btn btn-sm"
          onClick={async () => {
            try {
              const r = await api.post<{ url: string }>('/me/calendar-feeds', { kind: 'project', projectKey })
              setUrl(r.url)
              qc.invalidateQueries({ queryKey: ['calendar-feeds'] })
            } catch (e) {
              toast((e as Error).message)
            }
          }}
        >
          <CalendarPlus size={14} /> {t('Subscribe…')}
        </button>
      )}
      {url && (
        <Modal title={t('Subscribe to {key} in your calendar', { key: projectKey })} onClose={() => setUrl(null)} width={520}>
          <FeedAddress url={url} onDone={() => setUrl(null)} />
          <p className="faint small">{t('Your feeds are listed under Profile & settings → Calendar feeds.')}</p>
        </Modal>
      )}
    </>
  )
}
