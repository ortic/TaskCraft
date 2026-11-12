// Dashboards: pages people compose from widgets — issue lists, counts, breakdowns, sprint and
// flow reports, activity, their own work and notes. Private or shared with the workspace.
import { t } from '../i18n'
import { useState, type ReactNode } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { LayoutDashboard, Plus, Pencil, Trash, ArrowUp, ArrowDown, Maximize2, Minimize2, Share2, Check, ExternalLink } from 'lucide-react'
import { api, ago, qs, PRIORITIES, type HistoryEvent, type Issue } from '../api'
import { Avatar, BotAvatar, Confirm, Empty, ErrorBox, Loading, Modal, useMe, useProject, useProjects, useToast } from '../ui'
import { PriorityIcon, TypeIcon, priorityLabel } from '../icons'
import { BarList, CATEGORY_COLOR, SERIES } from '../components/charts'
import { RichView } from '../components/Editor'
import { describeEvent } from '../components/IssueView'
import { useOpenIssue } from '../components/Layout'
import { IssueLine } from './Home'
import { BurndownReport, CreatedReport, FlowReport, VelocityReport } from './Reports'
import './Reports.css'

type WidgetType = 'issues' | 'count' | 'breakdown' | 'burndown' | 'velocity' | 'flow' | 'created' | 'activity' | 'mywork' | 'note'

interface Widget {
  id: string
  type: WidgetType
  title: string
  config: { query?: string; limit?: number; by?: string; project?: string; text?: string; tone?: 'danger' | 'success' }
  wide: boolean
}

interface Dashboard {
  id: string
  ownerId: string | null
  ownerName: string | null
  name: string
  shared: boolean
  widgets: Widget[]
  updatedAt: string
  mine: boolean
}

const TYPES: { id: WidgetType; label: () => string; hint: () => string }[] = [
  { id: 'issues', label: () => t('Issue list'), hint: () => t('Issues matching a query') },
  { id: 'count', label: () => t('Count'), hint: () => t('How many issues match a query') },
  { id: 'breakdown', label: () => t('Breakdown'), hint: () => t('Issues matching a query, per status, person, priority, type, project, label or sprint') },
  { id: 'burndown', label: () => t('Sprint burndown'), hint: () => t('The running sprint of a scrum project') },
  { id: 'velocity', label: () => t('Velocity'), hint: () => t('Committed vs completed per sprint') },
  { id: 'flow', label: () => t('Cumulative flow'), hint: () => t('Issues per status over time') },
  { id: 'created', label: () => t('Created vs resolved'), hint: () => t('New and finished issues per day') },
  { id: 'activity', label: () => t('Activity'), hint: () => t('Recent changes in your projects') },
  { id: 'mywork', label: () => t('My work'), hint: () => t('Issues assigned to whoever looks at the dashboard') },
  { id: 'note', label: () => t('Note'), hint: () => t('Text in Markdown: links, goals, reminders') },
]

const BY: { id: string; label: () => string }[] = [
  { id: 'status', label: () => t('Status') },
  { id: 'assignee', label: () => t('Assignee') },
  { id: 'priority', label: () => t('Priority') },
  { id: 'type', label: () => t('Type') },
  { id: 'project', label: () => t('Project') },
  { id: 'labels', label: () => t('Labels') },
  { id: 'sprint', label: () => t('Sprint') },
]

const needsProject = (w: WidgetType) => ['burndown', 'velocity', 'flow', 'created'].includes(w)
const needsQuery = (w: WidgetType) => ['issues', 'count', 'breakdown'].includes(w)

export default function Dashboards() {
  const { id } = useParams()
  return id ? <DashboardView id={id} /> : <DashboardList />
}

function DashboardList() {
  const list = useQuery({ queryKey: ['dashboards'], queryFn: () => api.get<Dashboard[]>('/dashboards') })
  const nav = useNavigate()
  const toast = useToast()
  const [name, setName] = useState('')
  return (
    <div className="page">
      <div className="page-head">
        <h1><LayoutDashboard size={22} /> {t('Dashboards')}</h1>
      </div>
      <form
        className="row"
        style={{ marginBottom: 16, maxWidth: 520 }}
        onSubmit={async (e) => {
          e.preventDefault()
          if (!name.trim()) return
          try {
            const d = await api.post<{ id: string }>('/dashboards', { name })
            nav(`/dashboards/${d.id}`)
          } catch (err) {
            toast((err as Error).message)
          }
        }}
      >
        <input className="input" placeholder={t('New dashboard name, e.g. Team overview')} value={name} onChange={(e) => setName(e.target.value)} aria-label={t('New dashboard name')} />
        <button className="btn btn-primary" disabled={!name.trim()}><Plus size={15} /> {t('Create')}</button>
      </form>
      {list.isLoading && <Loading />}
      {list.data?.length === 0 && <Empty icon={<LayoutDashboard size={32} />} title={t('No dashboards yet')}>{t('A dashboard puts lists, counts and charts from all your projects on one page.')}</Empty>}
      <div className="grid-cards">
        {list.data?.map((d) => (
          <Link key={d.id} to={`/dashboards/${d.id}`} className="project-card">
            <div className="row">
              <LayoutDashboard size={20} />
              <strong style={{ flex: 1 }}>{d.name}</strong>
              {d.shared && <span className="badge"><Share2 size={11} /> {t('shared')}</span>}
            </div>
            <div className="faint small">{t('{count} widgets', { count: d.widgets.length })} · {d.mine ? t('yours') : d.ownerName} · {ago(d.updatedAt)}</div>
          </Link>
        ))}
      </div>
    </div>
  )
}

function DashboardView({ id }: { id: string }) {
  const qc = useQueryClient()
  const toast = useToast()
  const nav = useNavigate()
  const me = useMe().data?.user
  const q = useQuery({ queryKey: ['dashboard', id], queryFn: () => api.get<Dashboard>(`/dashboards/${id}`) })
  const [editing, setEditing] = useState(false)
  const [adding, setAdding] = useState<Widget | 'new' | null>(null)
  const [deleting, setDeleting] = useState(false)
  if (q.isLoading) return <Loading />
  if (q.error || !q.data) return <div className="page"><ErrorBox error={q.error ?? t('Not found')} /></div>
  const d = q.data
  const canEdit = d.mine || me?.role === 'owner' || me?.role === 'admin'
  const save = async (widgets: Widget[], extra: Record<string, unknown> = {}) => {
    qc.setQueryData<Dashboard>(['dashboard', id], (old) => (old ? { ...old, widgets, ...extra } : old))
    try {
      await api.patch(`/dashboards/${id}`, { widgets, ...extra })
    } catch (e) {
      toast((e as Error).message)
    }
    qc.invalidateQueries({ queryKey: ['dashboard', id] })
    qc.invalidateQueries({ queryKey: ['dashboards'] })
  }
  const move = (i: number, by: number) => {
    const w = [...d.widgets]
    const [x] = w.splice(i, 1)
    w.splice(Math.max(0, Math.min(w.length, i + by)), 0, x)
    void save(w)
  }
  return (
    <div className="page-wide">
      <div className="crumbs"><Link to="/dashboards">{t('Dashboards')}</Link></div>
      <div className="page-head">
        <h1>{d.name}</h1>
        {d.shared && <span className="badge"><Share2 size={11} /> {t('shared')}</span>}
        {!d.mine && <span className="faint small">{t('by {name}', { name: d.ownerName ?? '' })}</span>}
        <span className="spacer" />
        {canEdit && editing && (
          <>
            <label className="row small"><input type="checkbox" checked={d.shared} onChange={(e) => save(d.widgets, { shared: e.target.checked })} /> {t('Share with everyone')}</label>
            <button className="btn" onClick={() => setAdding('new')}><Plus size={15} /> {t('Add widget')}</button>
            <button className="btn btn-ghost btn-danger" onClick={() => setDeleting(true)}><Trash size={15} /> {t('Delete')}</button>
          </>
        )}
        {canEdit && (
          <button className={`btn ${editing ? 'btn-primary' : ''}`} onClick={() => setEditing((e) => !e)}>
            {editing ? <><Check size={15} /> {t('Done [[close]]')}</> : <><Pencil size={15} /> {t('Edit')}</>}
          </button>
        )}
      </div>
      {d.widgets.length === 0 && (
        <Empty icon={<LayoutDashboard size={32} />} title={t('This dashboard is empty')}>
          {canEdit && <button className="btn btn-primary" onClick={() => { setEditing(true); setAdding('new') }}><Plus size={15} /> {t('Add widget')}</button>}
        </Empty>
      )}
      <div className="dash-grid">
        {d.widgets.map((w, i) => (
          <section key={w.id} className={`panel dash-widget ${w.wide ? 'wide' : ''}`} aria-label={w.title || TYPES.find((x) => x.id === w.type)?.label()}>
            {(editing || w.title) && (
              <div className="panel-head">
                <h2>{w.title || TYPES.find((x) => x.id === w.type)?.label()}</h2>
                <span className="spacer" />
                {needsQuery(w.type) && !editing && (
                  <Link className="icon-btn icon-btn-sm" to={`/search?q=${encodeURIComponent(w.config.query ?? '')}`} title={t('Open as filter')} aria-label={t('Open as filter')}><ExternalLink size={13} /></Link>
                )}
                {editing && (
                  <>
                    <button className="icon-btn icon-btn-sm" aria-label={t('Move up')} title={t('Move up')} disabled={i === 0} onClick={() => move(i, -1)}><ArrowUp size={14} /></button>
                    <button className="icon-btn icon-btn-sm" aria-label={t('Move down')} title={t('Move down')} disabled={i === d.widgets.length - 1} onClick={() => move(i, 1)}><ArrowDown size={14} /></button>
                    <button className="icon-btn icon-btn-sm" aria-label={w.wide ? t('Make narrow') : t('Make wide')} title={w.wide ? t('Make narrow') : t('Make wide')} onClick={() => save(d.widgets.map((x) => (x.id === w.id ? { ...x, wide: !x.wide } : x)))}>
                      {w.wide ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
                    </button>
                    <button className="icon-btn icon-btn-sm" aria-label={t('Edit')} title={t('Edit')} onClick={() => setAdding(w)}><Pencil size={14} /></button>
                    <button className="icon-btn icon-btn-sm" aria-label={t('Remove')} title={t('Remove')} onClick={() => save(d.widgets.filter((x) => x.id !== w.id))}><Trash size={14} /></button>
                  </>
                )}
              </div>
            )}
            <div className="dash-body">
              <WidgetBody w={w} />
            </div>
          </section>
        ))}
      </div>
      {adding && (
        <WidgetDialog
          widget={adding === 'new' ? null : adding}
          onClose={() => setAdding(null)}
          onSave={(w) => {
            setAdding(null)
            void save(adding === 'new' ? [...d.widgets, w] : d.widgets.map((x) => (x.id === w.id ? w : x)))
          }}
        />
      )}
      {deleting && (
        <Confirm
          title={t('Delete {name}?', { name: d.name })}
          body={<p>{t('The dashboard goes away for everyone it was shared with. The issues stay.')}</p>}
          confirmLabel={t('Delete')}
          danger
          onClose={() => setDeleting(false)}
          onConfirm={async () => {
            await api.del(`/dashboards/${id}`)
            qc.invalidateQueries({ queryKey: ['dashboards'] })
            nav('/dashboards')
          }}
        />
      )}
    </div>
  )
}

function WidgetBody({ w }: { w: Widget }) {
  switch (w.type) {
    case 'issues':
      return <IssuesWidget query={w.config.query ?? ''} limit={w.config.limit ?? 10} />
    case 'count':
      return <CountWidget query={w.config.query ?? ''} tone={w.config.tone} />
    case 'breakdown':
      return <BreakdownWidget query={w.config.query ?? ''} by={w.config.by ?? 'status'} />
    case 'activity':
      return <ActivityWidget project={w.config.project} />
    case 'mywork':
      return <IssuesWidget query="assignee = me() and category != done order by priority desc, due" limit={w.config.limit ?? 10} />
    case 'note':
      return <div className="dash-note"><RichView doc={noteDoc(w.config.text ?? '')} label={w.title || t('Note')} /></div>
    default:
      return <ReportWidget type={w.type} project={w.config.project} />
  }
}

/** A note's Markdown as a simple document (paragraphs, bullets, links stay text). */
function noteDoc(text: string) {
  const lines = text.split('\n')
  const content: any[] = []
  let list: any[] | null = null
  for (const l of lines) {
    const m = /^\s*[-*]\s+(.*)$/.exec(l)
    if (m) {
      list ??= []
      list.push({ type: 'listItem', content: [{ type: 'paragraph', content: m[1] ? [{ type: 'text', text: m[1] }] : [] }] })
      continue
    }
    if (list) {
      content.push({ type: 'bulletList', content: list })
      list = null
    }
    const h = /^(#{1,3})\s+(.*)$/.exec(l)
    if (h) content.push({ type: 'heading', attrs: { level: h[1].length + 1 }, content: [{ type: 'text', text: h[2] }] })
    else content.push(l.trim() ? { type: 'paragraph', content: [{ type: 'text', text: l }] } : { type: 'paragraph' })
  }
  if (list) content.push({ type: 'bulletList', content: list })
  return { type: 'doc', content: content.length ? content : [{ type: 'paragraph' }] }
}

function IssuesWidget({ query, limit }: { query: string; limit: number }) {
  const q = useQuery({ queryKey: ['search', 'widget', query, limit], queryFn: () => api.get<{ total: number; issues: Issue[] }>(`/search${qs({ q: query, limit })}`) })
  if (q.isLoading) return <Loading />
  if (q.error) return <ErrorBox error={q.error} />
  if (!q.data?.issues.length) return <div className="faint small" style={{ padding: 12 }}>{t('Nothing matches.')}</div>
  return (
    <div>
      {q.data.issues.map((i) => <IssueLine key={i.id} issue={i} />)}
      {q.data.total > q.data.issues.length && (
        <Link className="small" style={{ display: 'block', padding: '8px 14px' }} to={`/search?q=${encodeURIComponent(query)}`}>{t('{count} more', { count: q.data.total - q.data.issues.length })}</Link>
      )}
    </div>
  )
}

function CountWidget({ query, tone }: { query: string; tone?: 'danger' | 'success' }) {
  const q = useQuery({ queryKey: ['search', 'count', query], queryFn: () => api.get<{ total: number }>(`/search${qs({ q: query, limit: 1 })}`) })
  return (
    <Link to={`/search?q=${encodeURIComponent(query)}`} className="dash-count" style={{ color: tone === 'danger' && q.data?.total ? 'var(--danger)' : tone === 'success' ? 'var(--success)' : 'var(--text)' }}>
      {q.isLoading ? '…' : (q.data?.total ?? '–')}
      <span className="faint small" style={{ fontFamily: 'var(--mono)' }}>{query}</span>
    </Link>
  )
}

interface Groups {
  total: number
  groups: { key: string; label: string | null; count: number; points: number; color: string | null; extra: string | null }[]
}

function BreakdownWidget({ query, by }: { query: string; by: string }) {
  const q = useQuery({ queryKey: ['search', 'groups', query, by], queryFn: () => api.get<Groups>(`/search/groups${qs({ q: query, by })}`) })
  const open = useNavigate()
  if (q.isLoading) return <Loading />
  if (q.error) return <ErrorBox error={q.error} />
  if (!q.data?.groups.length) return <div className="faint small" style={{ padding: 12 }}>{t('Nothing matches.')}</div>
  const label = (g: Groups['groups'][number]): string => {
    if (by === 'priority') return priorityLabel((g.label ?? 'none') as (typeof PRIORITIES)[number])
    if (by === 'category') return g.extra === 'todo' ? t('To do') : g.extra === 'in_progress' ? t('In progress') : t('Done')
    if (!g.label) return by === 'assignee' ? t('Unassigned') : by === 'sprint' ? t('Backlog') : by === 'labels' ? t('No label') : '–'
    return g.label
  }
  const color = (g: Groups['groups'][number], i: number) =>
    by === 'status' || by === 'category' ? CATEGORY_COLOR[(g.extra ?? 'todo') as keyof typeof CATEGORY_COLOR] ?? SERIES[0] : SERIES[i % SERIES.length]
  return (
    <div style={{ padding: '4px 12px 10px' }}>
      <BarList
        summary={t('{count} issues by {field}', { count: q.data.total, field: BY.find((b) => b.id === by)?.label() ?? by })}
        nameHeader={BY.find((b) => b.id === by)?.label() ?? by}
        valueHeader={t('Issues')}
        shareOf={q.data.total}
        rows={q.data.groups.map((g, i) => ({
          id: g.key || `none-${i}`,
          name: label(g),
          label: (
            <button className="ps-link-btn" style={{ border: 0, background: 'none', padding: 0, cursor: 'pointer', color: 'inherit', display: 'inline-flex', gap: 6, alignItems: 'center' }} onClick={() => open(`/search?q=${encodeURIComponent(drill(query, by, g))}`)}>
              {by === 'assignee' && g.label && <Avatar name={g.label} id={g.key} size={18} decorative />}
              {by === 'priority' && <PriorityIcon priority={(g.label ?? 'none') as (typeof PRIORITIES)[number]} />}
              {by === 'type' && g.color && <TypeIcon icon={g.extra ?? 'task'} color={g.color} size={14} />}
              {label(g)}
            </button>
          ),
          segments: [{ value: g.count, color: color(g, i), label: t('Issues') }],
        }))}
      />
    </div>
  )
}

/** The query of one bar (adds the group as a condition). */
function drill(query: string, by: string, g: Groups['groups'][number]): string {
  const q = (v: string) => (/^[\p{L}\p{N}_\-.@]+$/u.test(v) ? v : `"${v.replace(/"/g, '\\"')}"`)
  const cond =
    by === 'assignee' ? (g.key ? `assignee = ${q(g.key)}` : 'assignee is empty')
    : by === 'labels' ? (g.label ? `labels = ${q(g.label)}` : 'labels is empty')
    : by === 'sprint' ? (g.label ? `sprint = ${q(g.label)}` : 'sprint is empty')
    : by === 'project' ? `project = ${g.key}`
    : by === 'category' ? `category = ${g.extra}`
    : `${by} = ${q(g.label ?? '')}`
  const base = query.replace(/\s+order\s+by\s+.*$/i, '').trim()
  return base ? `(${base}) and ${cond}` : cond
}

function ActivityWidget({ project }: { project?: string }) {
  const q = useQuery({ queryKey: ['activity', project ?? ''], queryFn: () => api.get<HistoryEvent[]>(`/activity${qs({ project })}`) })
  const open = useOpenIssue()
  if (q.isLoading) return <Loading />
  return (
    <div style={{ padding: '4px 14px', maxHeight: 420, overflowY: 'auto' }}>
      {(q.data ?? []).slice(0, 20).map((e) => (
        <div key={e.id} className="activity-item small">
          {e.actor ? <Avatar name={e.actor.name} id={e.actor.id} size={22} /> : e.new?.by ? <Avatar name={e.new.by} size={22} /> : <BotAvatar size={22} />}
          <div className="ai-body">
            <strong>{e.actor?.name ?? e.new?.by ?? t('Automation')}</strong> {describeEvent(e)}{' '}
            <a href={`?issue=${e.key}`} onClick={(ev) => { ev.preventDefault(); open(e.key!) }}><span className="issue-key">{e.key}</span></a>
            <div className="faint">{ago(e.at)}</div>
          </div>
        </div>
      ))}
    </div>
  )
}

function ReportWidget({ type, project }: { type: WidgetType; project?: string }) {
  const p = useProject(project)
  if (!project) return <div className="faint small" style={{ padding: 12 }}>{t('Choose a project for this widget.')}</div>
  if (p.isLoading) return <Loading />
  if (!p.data) return <ErrorBox error={p.error ?? t('Not found')} />
  const body: ReactNode =
    type === 'burndown' ? <BurndownReport project={p.data} />
    : type === 'velocity' ? <VelocityReport project={p.data} />
    : type === 'flow' ? <FlowReport project={p.data} />
    : <CreatedReport project={p.data} />
  return <div className="dash-report">{body}</div>
}

function WidgetDialog({ widget, onClose, onSave }: { widget: Widget | null; onClose: () => void; onSave: (w: Widget) => void }) {
  const projects = useProjects()
  const [type, setType] = useState<WidgetType>(widget?.type ?? 'issues')
  const [title, setTitle] = useState(widget?.title ?? '')
  const [query, setQuery] = useState(widget?.config.query ?? 'category != done order by priority desc')
  const [by, setBy] = useState(widget?.config.by ?? 'status')
  const [limit, setLimit] = useState(widget?.config.limit ?? 10)
  const [project, setProject] = useState(widget?.config.project ?? '')
  const [text, setText] = useState(widget?.config.text ?? '')
  const [tone, setTone] = useState(widget?.config.tone ?? '')
  const ok = (!needsProject(type) || !!project) && (type !== 'note' || !!text.trim())
  return (
    <Modal
      title={widget ? t('Edit widget') : t('Add widget')}
      onClose={onClose}
      width={600}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button
            className="btn btn-primary"
            disabled={!ok}
            onClick={() =>
              onSave({
                id: widget?.id ?? crypto.randomUUID(),
                type,
                title,
                wide: widget?.wide ?? ['issues', 'mywork', 'burndown', 'flow', 'created'].includes(type),
                config: {
                  ...(needsQuery(type) ? { query } : {}),
                  ...(type === 'breakdown' ? { by } : {}),
                  ...(type === 'issues' || type === 'mywork' ? { limit } : {}),
                  ...(needsProject(type) || type === 'activity' ? (project ? { project } : {}) : {}),
                  ...(type === 'note' ? { text } : {}),
                  ...(type === 'count' && tone ? { tone: tone as 'danger' | 'success' } : {}),
                },
              })
            }
          >{widget ? t('Save') : t('Add')}</button>
        </>
      }
    >
      <div className="field">
        <span className="field-label">{t('Kind')}</span>
        <div className="template-grid">
          {TYPES.map((x) => (
            <button key={x.id} type="button" className={`template ${type === x.id ? 'on' : ''}`} aria-pressed={type === x.id} onClick={() => setType(x.id)}>
              <strong>{x.label()}</strong>
              <span className="faint small">{x.hint()}</span>
            </button>
          ))}
        </div>
      </div>
      <div className="field">
        <label htmlFor="w-title">{t('Title (optional)')}</label>
        <input id="w-title" className="input" value={title} onChange={(e) => setTitle(e.target.value)} />
      </div>
      {needsQuery(type) && (
        <div className="field">
          <label htmlFor="w-query">{t('Query (TQL)')}</label>
          <input id="w-query" className="input query-input" value={query} onChange={(e) => setQuery(e.target.value)} placeholder="project = WEB and category != done" />
        </div>
      )}
      {type === 'breakdown' && (
        <div className="field">
          <label htmlFor="w-by">{t('Per')}</label>
          <select id="w-by" className="select" value={by} onChange={(e) => setBy(e.target.value)}>{BY.map((b) => <option key={b.id} value={b.id}>{b.label()}</option>)}</select>
        </div>
      )}
      {(type === 'issues' || type === 'mywork') && (
        <div className="field">
          <label htmlFor="w-limit">{t('How many issues')}</label>
          <select id="w-limit" className="select" style={{ width: 'auto' }} value={limit} onChange={(e) => setLimit(Number(e.target.value))}>{[5, 10, 20, 50].map((n) => <option key={n} value={n}>{n}</option>)}</select>
        </div>
      )}
      {type === 'count' && (
        <div className="field">
          <label htmlFor="w-tone">{t('Colour')}</label>
          <select id="w-tone" className="select" style={{ width: 'auto' }} value={tone} onChange={(e) => setTone(e.target.value)}>
            <option value="">{t('Neutral')}</option>
            <option value="danger">{t('Red when above zero (problems)')}</option>
            <option value="success">{t('Green (good news)')}</option>
          </select>
        </div>
      )}
      {(needsProject(type) || type === 'activity') && (
        <div className="field">
          <label htmlFor="w-project">{t('Project')}</label>
          <select id="w-project" className="select" value={project} onChange={(e) => setProject(e.target.value)}>
            <option value="">{type === 'activity' ? t('All my projects') : t('Choose a project…')}</option>
            {(projects.data ?? []).filter((p) => !p.archivedAt && (type !== 'burndown' && type !== 'velocity' ? true : p.kind === 'scrum')).map((p) => <option key={p.id} value={p.key}>{p.name}</option>)}
          </select>
        </div>
      )}
      {type === 'note' && (
        <div className="field">
          <label htmlFor="w-text">{t('Text (Markdown: # headings, - lists)')}</label>
          <textarea id="w-text" className="textarea" rows={6} value={text} onChange={(e) => setText(e.target.value)} />
        </div>
      )}
    </Modal>
  )
}
