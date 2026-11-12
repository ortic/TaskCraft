import { t } from '../i18n'
import { useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useQueryClient } from '@tanstack/react-query'
import { Plus, Star, Archive, ArchiveRestore, Kanban, ListTodo, Bug, Briefcase } from 'lucide-react'
import { api } from '../api'
import { Empty, Loading, Modal, useMe, useProjects, useToast } from '../ui'
import { PROJECT_COLORS, PROJECT_ICONS, ProjectIcon } from '../icons'

export default function Projects() {
  const projects = useProjects()
  const me = useMe().data
  const qc = useQueryClient()
  const [creating, setCreating] = useState(false)
  const [archived, setArchived] = useState(false)
  const [q, setQ] = useState('')
  const admin = me?.user?.role === 'owner' || me?.user?.role === 'admin'
  const list = (projects.data ?? []).filter((p) => (archived ? !!p.archivedAt : !p.archivedAt) && (!q || `${p.name} ${p.key}`.toLowerCase().includes(q.toLowerCase())))
  return (
    <div className="page">
      <div className="page-head">
        <h1>{t('Projects')}</h1>
        <span className="spacer" />
        <input className="input" style={{ width: 220 }} placeholder={t('Filter projects…')} value={q} onChange={(e) => setQ(e.target.value)} aria-label={t('Filter projects')} />
        <button className={`btn ${archived ? 'active' : ''}`} onClick={() => setArchived((a) => !a)}><Archive size={15} /> {t('Archived')}</button>
        {admin && (
          <Link className="btn" to="/admin/import" title={t('Import a backup from another tracker')}><ArchiveRestore size={15} /> {t('Import')}</Link>
        )}
        {me?.user?.role !== 'guest' && (
          <button className="btn btn-primary" onClick={() => setCreating(true)}><Plus size={15} /> {t('Create project')}</button>
        )}
      </div>
      {projects.isLoading && <Loading />}
      {projects.data && list.length === 0 && (
        <Empty icon={<Kanban size={34} />} title={archived ? t('No archived projects') : t('No projects yet')}>
          {!archived && t('A project holds issues, a workflow and boards. Start one from a template.')}
          {!archived && admin && (
            <p>
              <Link to="/admin/import">{t('Moving from another tracker? Import its backup.')}</Link>
            </p>
          )}
        </Empty>
      )}
      <div className="grid-cards">
        {list.map((p) => {
          const total = (p.openCount ?? 0) + (p.doneCount ?? 0)
          const pct = total ? Math.round(((p.doneCount ?? 0) / total) * 100) : 0
          return (
            <Link key={p.id} to={`/projects/${p.key}`} className="project-card">
              <div className="row">
                <ProjectIcon icon={p.icon} color={p.color} size={34} />
                <div style={{ minWidth: 0, flex: 1 }}>
                  <div style={{ fontWeight: 650, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{p.name}</div>
                  <div className="faint small">{p.key} · {p.kind === 'scrum' ? t('Scrum') : p.kind === 'kanban' ? t('Kanban') : t('Tracker')}</div>
                </div>
                <button
                  className={`icon-btn ${p.starred ? 'active' : ''}`}
                  aria-label={p.starred ? t('Unstar') : t('Star')}
                  title={p.starred ? t('Unstar') : t('Star')}
                  onClick={async (e) => {
                    e.preventDefault()
                    await api.put(`/projects/${p.key}/star`, { starred: !p.starred })
                    qc.invalidateQueries({ queryKey: ['projects'] })
                  }}
                >
                  <Star size={16} fill={p.starred ? 'currentColor' : 'none'} />
                </button>
              </div>
              {p.description && <div className="muted small" style={{ display: '-webkit-box', WebkitLineClamp: 2, WebkitBoxOrient: 'vertical', overflow: 'hidden' }}>{p.description}</div>}
              <div className="progress"><span className="p-done" style={{ width: `${pct}%` }} /></div>
              <div className="row small faint">
                <span>{t('{open} open · {done} done', { open: p.openCount ?? 0, done: p.doneCount ?? 0 })}</span>
                <span className="spacer" />
                {p.activeSprint && <span className="badge badge-accent">{p.activeSprint}</span>}
              </div>
              {p.leadName && <div className="faint small">{t('Lead: {name}', { name: p.leadName })}</div>}
            </Link>
          )
        })}
      </div>
      {creating && <CreateProject onClose={() => setCreating(false)} />}
    </div>
  )
}

const TEMPLATES = [
  { id: 'scrum', icon: <ListTodo size={18} />, name: () => t('Scrum'), desc: () => t('Plan sprints from a ranked backlog, track them on a board, review burndown and velocity.') },
  { id: 'kanban', icon: <Kanban size={18} />, name: () => t('Kanban'), desc: () => t('A continuous flow with WIP limits; measure cycle time and cumulative flow.') },
  { id: 'tracker', icon: <Bug size={18} />, name: () => t('Issue tracker'), desc: () => t('Bugs, requests and questions with severity and environment fields.') },
  { id: 'business', icon: <Briefcase size={18} />, name: () => t('Work plan'), desc: () => t('Tasks and milestones with dates and budgets for teams outside software.') },
]

export function suggestKey(name: string): string {
  const words = name.toUpperCase().replace(/[^A-Z0-9 ]/g, ' ').split(/\s+/).filter(Boolean)
  let k = words.length > 1 ? words.map((w) => w[0]).join('') : (words[0] ?? '').slice(0, 4)
  if (!/^[A-Z]/.test(k)) k = 'P' + k
  return k.slice(0, 6)
}

function CreateProject({ onClose }: { onClose: () => void }) {
  const [name, setName] = useState('')
  const [key, setKey] = useState('')
  const [keyTouched, setKeyTouched] = useState(false)
  const [template, setTemplate] = useState('scrum')
  const [icon, setIcon] = useState('rocket')
  const [color, setColor] = useState(PROJECT_COLORS[0])
  const [description, setDescription] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const nav = useNavigate()
  const qc = useQueryClient()
  const toast = useToast()
  const finalKey = keyTouched ? key : suggestKey(name)
  return (
    <Modal
      title={t('Create project')}
      onClose={onClose}
      width={640}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button
            className="btn btn-primary"
            disabled={busy || !name.trim() || finalKey.length < 2}
            onClick={async () => {
              setBusy(true)
              setErr('')
              try {
                const r = await api.post<{ key: string }>('/projects', { name, key: finalKey, template, icon, color, description })
                qc.invalidateQueries({ queryKey: ['projects'] })
                toast(t('Project {name} created', { name }))
                onClose()
                nav(`/projects/${r.key}`)
              } catch (e) {
                setErr((e as Error).message)
              } finally {
                setBusy(false)
              }
            }}
          >{t('Create project')}</button>
        </>
      }
    >
      <div className="field">
        <span className="field-label">{t('Template')}</span>
        <div className="template-grid">
          {TEMPLATES.map((tp) => (
            <button key={tp.id} type="button" className={`template ${template === tp.id ? 'on' : ''}`} onClick={() => setTemplate(tp.id)} aria-pressed={template === tp.id}>
              <div className="row" style={{ fontWeight: 600 }}>{tp.icon} {tp.name()}</div>
              <div className="faint small">{tp.desc()}</div>
            </button>
          ))}
        </div>
      </div>
      <div className="row" style={{ alignItems: 'flex-start', gap: 12 }}>
        <div className="field" style={{ flex: 1 }}>
          <label htmlFor="pn">{t('Name')}</label>
          <input id="pn" className="input" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder={t('e.g. Mobile app')} />
        </div>
        <div className="field" style={{ width: 130 }}>
          <label htmlFor="pk">{t('Key')}</label>
          <input id="pk" className="input" value={finalKey} onChange={(e) => { setKeyTouched(true); setKey(e.target.value.toUpperCase().replace(/[^A-Z0-9]/g, '').slice(0, 10)) }} />
          <span className="field-hint">{t('Issue keys: {key}-1', { key: finalKey || 'KEY' })}</span>
        </div>
      </div>
      <div className="field">
        <label htmlFor="pd">{t('Description (optional)')}</label>
        <input id="pd" className="input" value={description} onChange={(e) => setDescription(e.target.value)} />
      </div>
      <div className="row" style={{ alignItems: 'flex-start', gap: 20 }}>
        <div className="field">
          <span className="field-label">{t('Colour')}</span>
          <div className="swatches">
            {PROJECT_COLORS.map((c) => <button key={c} type="button" className={`swatch ${c === color ? 'on' : ''}`} style={{ background: c }} aria-label={c} onClick={() => setColor(c)} />)}
          </div>
        </div>
        <div className="field" style={{ flex: 1 }}>
          <span className="field-label">{t('Icon')}</span>
          <div className="icon-grid">
            {Object.keys(PROJECT_ICONS).map((i) => (
              <button key={i} type="button" className={i === icon ? 'on' : ''} onClick={() => setIcon(i)} aria-label={i} title={i}>
                <ProjectIcon icon={i} color={color} size={24} />
              </button>
            ))}
          </div>
        </div>
      </div>
      {err && <p className="error-text">{err}</p>}
    </Modal>
  )
}
