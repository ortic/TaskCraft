import { t } from '../i18n'
import { useMemo, useState } from 'react'
import { Link, useParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Search, GripVertical, Ellipsis, Play, Flag, Pencil, Trash, ChevronDown, ChevronRight, Target, Layers, Ban, Lock } from 'lucide-react'
import { api, daysUntil, shortDate, isoDate, type Issue, type Project, type Sprint } from '../api'
import { Avatar, Confirm, Empty, ErrorBox, Loading, Lozenge, Modal, PopButton, Unassigned, useProject, useToast } from '../ui'
import { PriorityIcon, TypeIcon } from '../icons'
import { useOpenIssue } from '../components/Layout'
import { refreshIssues } from '../components/IssueView'
import { CompleteSprint } from './Board'

interface BacklogData {
  canEdit: boolean
  sprints: (Sprint & { issues: Issue[] })[]
  backlog: Issue[]
  epics: Issue[]
}

type Drop = { container: string; index: number }

export default function Backlog() {
  const { key } = useParams()
  const project = useProject(key)
  const data = useQuery({ queryKey: ['backlog', key?.toUpperCase()], queryFn: () => api.get<BacklogData>(`/projects/${key}/backlog`), enabled: !!key })
  const velocity = useQuery({ queryKey: ['report', 'velocity', key], queryFn: () => api.get<{ average: number; unit: string; sprints: unknown[] }>(`/projects/${key}/reports/velocity?count=3`), enabled: !!key })
  if (project.isLoading || data.isLoading) return <Loading />
  if (project.error || data.error) return <div className="page"><ErrorBox error={project.error ?? data.error} /></div>
  if (!project.data || !data.data) return null
  return <BacklogView project={project.data} data={data.data} velocity={velocity.data?.sprints.length ? velocity.data.average : null} />
}

function BacklogView({ project, data, velocity }: { project: Project; data: BacklogData; velocity: number | null }) {
  const qc = useQueryClient()
  const toast = useToast()
  const open = useOpenIssue()
  const [text, setText] = useState('')
  const [epic, setEpic] = useState<string | null>(null)
  const [showEpics, setShowEpics] = useState(true)
  const [drag, setDrag] = useState<Issue | null>(null)
  const [drop, setDrop] = useState<Drop | null>(null)
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set())
  const [editSprint, setEditSprint] = useState<Sprint | 'new' | null>(null)
  const [starting, setStarting] = useState<Sprint | null>(null)
  const [completing, setCompleting] = useState<(Sprint & { issues: Issue[] }) | null>(null)
  const [deleting, setDeleting] = useState<Sprint | null>(null)
  const queryKey = ['backlog', project.key]
  const usePoints = (project.settings.estimation ?? 'points') === 'points'

  const filter = (list: Issue[]) =>
    list.filter((i) => (!epic || (epic === 'none' ? !i.parent : i.parent?.id === epic)) && (!text || `${i.key} ${i.title} ${i.labels.join(' ')}`.toLowerCase().includes(text.toLowerCase())))

  const containers = useMemo(() => {
    const m = new Map<string, Issue[]>()
    data.sprints.forEach((s) => m.set(s.id, [...s.issues].sort((a, b) => a.rank - b.rank)))
    m.set('backlog', [...data.backlog].sort((a, b) => a.rank - b.rank))
    return m
  }, [data])

  const moveTo = async (issue: Issue, target: Drop) => {
    const list = filter(containers.get(target.container) ?? []).filter((i) => i.id !== issue.id)
    const after = list[target.index - 1]?.id ?? null
    const before = after ? null : (list[target.index]?.id ?? null)
    const sprintId = target.container === 'backlog' ? null : target.container
    const sameContainer = (issue.sprint?.id ?? 'backlog') === target.container
    // optimistic
    qc.setQueryData<BacklogData>(queryKey, (old) => {
      if (!old) return old
      const ranks = [list[target.index - 1]?.rank, list[target.index]?.rank]
      const rank = ranks[0] != null && ranks[1] != null ? (ranks[0] + ranks[1]) / 2 : ranks[0] != null ? ranks[0] + 1 : ranks[1] != null ? ranks[1] - 1 : issue.rank
      const sprint = sprintId ? old.sprints.find((s) => s.id === sprintId) : null
      const moved: Issue = { ...issue, rank, sprint: sprint ? { id: sprint.id, name: sprint.name, state: sprint.state } : null }
      const strip = (l: Issue[]) => l.filter((i) => i.id !== issue.id)
      return {
        ...old,
        backlog: target.container === 'backlog' ? [...strip(old.backlog), moved] : strip(old.backlog),
        sprints: old.sprints.map((s) => ({ ...s, issues: s.id === target.container ? [...strip(s.issues), moved] : strip(s.issues) })),
      }
    })
    try {
      await api.post(`/issues/${issue.key}/move`, { ...(sameContainer ? {} : { sprintId }), after, before })
    } catch (e) {
      toast((e as Error).message)
    }
    qc.invalidateQueries({ queryKey })
    refreshIssues(qc)
  }

  const dragProps = (container: string, list: Issue[]) => ({
    onDragOver: (e: React.DragEvent<HTMLDivElement>) => {
      if (!drag) return
      e.preventDefault()
      const rows = [...e.currentTarget.querySelectorAll<HTMLElement>('.issue-row')]
      let index = rows.length
      for (let k = 0; k < rows.length; k++) {
        const r = rows[k].getBoundingClientRect()
        if (e.clientY < r.top + r.height / 2) {
          index = k
          break
        }
      }
      if (!drop || drop.container !== container || drop.index !== index) setDrop({ container, index })
    },
    onDragLeave: (e: React.DragEvent<HTMLDivElement>) => {
      if (!e.currentTarget.contains(e.relatedTarget as Node)) setDrop(null)
    },
    onDrop: (e: React.DragEvent<HTMLDivElement>) => {
      e.preventDefault()
      if (drag && drop) {
        const before = list.slice(0, drop.index).filter((i) => i.id !== drag.id).length
        void moveTo(drag, { container, index: before })
      }
      setDrag(null)
      setDrop(null)
    },
  })

  const row = (i: Issue, container: string, index: number) => (
    <div
      key={i.id}
      className={`issue-row ${drag?.id === i.id ? 'dragging' : ''} ${drop?.container === container && drop.index === index ? 'drop-before' : ''}`}
      draggable={data.canEdit}
      onDragStart={(e) => {
        e.dataTransfer.effectAllowed = 'move'
        e.dataTransfer.setData('text/plain', i.key)
        setDrag(i)
      }}
      onDragEnd={() => {
        setDrag(null)
        setDrop(null)
      }}
      onClick={() => open(i.key)}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => e.key === 'Enter' && open(i.key)}
    >
      {data.canEdit && <span className="grip" aria-hidden><GripVertical size={14} /></span>}
      <TypeIcon icon={i.type.icon} color={i.type.color} title={i.type.name} />
      <span className="issue-key" style={{ minWidth: 62, textDecoration: i.status.category === 'done' ? 'line-through' : undefined }}>{i.key}</span>
      <span className="ir-title">{i.title}</span>
      {i.security && <span className="ic-private" title={t('Private ({name})', { name: i.security.name })}><Lock size={12} aria-label={t('Private ({name})', { name: i.security.name })} /></span>}
      {i.blocked && i.status.category !== 'done' && <span title={t('Blocked')} style={{ color: 'var(--danger)', display: 'inline-flex' }}><Ban size={13} /></span>}
      {i.parent && (
        <span className="label-chip" style={{ background: `color-mix(in srgb, ${i.parent.color ?? '#8b5cf6'} 16%, transparent)`, color: 'var(--text)', maxWidth: 160, overflow: 'hidden', textOverflow: 'ellipsis' }} title={i.parent.title}>
          {i.parent.title}
        </span>
      )}
      {i.labels.slice(0, 2).map((l) => <span key={l} className="label-chip">{l}</span>)}
      <Lozenge name={i.status.name} category={i.status.category} />
      <PriorityIcon priority={i.priority} />
      {usePoints && <span className="ic-points" style={{ minWidth: 26, height: 20, borderRadius: 10, background: 'var(--bg-hover)', fontSize: 12, fontWeight: 600, display: 'inline-flex', alignItems: 'center', justifyContent: 'center' }} title={t('Story points')}>{i.storyPoints ?? '–'}</span>}
      {i.assignee ? <Avatar name={i.assignee.name} id={i.assignee.id} size={22} /> : <Unassigned size={22} />}
    </div>
  )

  const sums = (list: Issue[]) => {
    const w = (i: Issue) => (usePoints ? (i.storyPoints ?? 0) : 1)
    return {
      todo: list.filter((i) => i.status.category === 'todo').reduce((s, i) => s + w(i), 0),
      progress: list.filter((i) => i.status.category === 'in_progress').reduce((s, i) => s + w(i), 0),
      done: list.filter((i) => i.status.category === 'done').reduce((s, i) => s + w(i), 0),
    }
  }
  const hasActive = data.sprints.some((s) => s.state === 'active')

  const box = (container: string, title: React.ReactNode, list: Issue[], head: React.ReactNode, sprint?: Sprint & { issues: Issue[] }) => {
    const shown = filter(list)
    const isCollapsed = collapsed.has(container)
    const s = sums(list)
    const total = s.todo + s.progress + s.done
    return (
      <section key={container} className={`sprint-box ${drop?.container === container ? 'drop' : ''}`}>
        <div className="sprint-head">
          <button className="icon-btn icon-btn-sm" aria-expanded={!isCollapsed} aria-label={isCollapsed ? t('Expand') : t('Collapse')} onClick={() => setCollapsed((c) => { const n = new Set(c); if (n.has(container)) n.delete(container); else n.add(container); return n })}>
            {isCollapsed ? <ChevronRight size={15} /> : <ChevronDown size={15} />}
          </button>
          <h3>{title}</h3>
          <span className="faint small">{t('{count} issues', { count: list.length })}</span>
          {head}
          <span className="spacer" />
          <span className="sprint-sums" title={usePoints ? t('Story points: to do, in progress, done') : t('Issues: to do, in progress, done')}>
            <span className="sum sum-todo">{s.todo}</span>
            <span className="sum sum-progress">{s.progress}</span>
            <span className="sum sum-done">{s.done}</span>
          </span>
          {sprint && velocity != null && usePoints && sprint.state === 'future' && total > 0 && (
            <span className={`badge ${total > velocity * 1.15 ? 'badge-warning' : ''}`} title={t('Average completed in the last sprints')}>
              {t('{planned} of ~{usual} pts', { planned: total, usual: Math.round(velocity) })}
            </span>
          )}
          {sprint && data.canEdit && sprint.state === 'future' && (
            <button className="btn btn-sm" disabled={hasActive} title={hasActive ? t('Complete the running sprint first') : undefined} onClick={() => setStarting(sprint)}>
              <Play size={13} /> {t('Start sprint')}
            </button>
          )}
          {sprint && data.canEdit && sprint.state === 'active' && (
            <button className="btn btn-sm" onClick={() => setCompleting(sprint)}><Flag size={13} /> {t('Complete sprint')}</button>
          )}
          {sprint && data.canEdit && (
            <PopButton className="icon-btn icon-btn-sm" title={t('Sprint actions')} label={<Ellipsis size={15} />} align="end" width={200}>
              {(close) => (
                <>
                  <button className="menu-item" onClick={() => { close(); setEditSprint(sprint) }}><Pencil size={14} /> {t('Edit sprint')}</button>
                  {sprint.state === 'future' && <button className="menu-item danger" onClick={() => { close(); setDeleting(sprint) }}><Trash size={14} /> {t('Delete sprint')}</button>}
                </>
              )}
            </PopButton>
          )}
        </div>
        {!isCollapsed && (
          <div className="issue-rows" {...dragProps(container, shown)}>
            {shown.map((i, k) => row(i, container, k))}
            {drop?.container === container && drop.index >= shown.length && shown.length > 0 && <div className="drop-line" />}
            {shown.length === 0 && (
              <div className="faint small" style={{ padding: '12px 14px' }}>
                {container === 'backlog' ? t('The backlog is empty. Create issues or move them out of sprints.') : t('Plan this sprint by dragging issues here from the backlog.')}
              </div>
            )}
            {data.canEdit && <InlineCreate project={project} sprintId={container === 'backlog' ? null : container} parentId={epic && epic !== 'none' ? epic : undefined} />}
          </div>
        )}
      </section>
    )
  }

  return (
    <div className="page-wide">
      <div className="crumbs"><Link to="/projects">{t('Projects')}</Link><span>/</span><Link to={`/projects/${project.key}`}>{project.name}</Link></div>
      <div className="page-head">
        <h1>{t('Backlog')}</h1>
        <span className="spacer" />
        <div className="row" style={{ position: 'relative' }}>
          <Search size={14} className="faint" style={{ position: 'absolute', left: 9 }} />
          <input className="input input-sm" style={{ width: 220, paddingLeft: 28 }} placeholder={t('Filter issues…')} value={text} onChange={(e) => setText(e.target.value)} aria-label={t('Filter issues')} />
        </div>
        <button className={`btn btn-sm ${showEpics ? 'active' : ''}`} onClick={() => setShowEpics((s) => !s)} aria-pressed={showEpics}><Layers size={14} /> {t('Epics')}</button>
        {data.canEdit && project.kind === 'scrum' && <button className="btn btn-sm" onClick={() => setEditSprint('new')}><Plus size={14} /> {t('Create sprint')}</button>}
      </div>
      <div className={`backlog ${showEpics ? 'with-epics' : ''}`}>
        {showEpics && (
          <aside className="epic-panel" aria-label={t('Epics')}>
            <div className="menu-head">{t('Epics')}</div>
            <button className={`epic-item ${epic === null ? 'on' : ''}`} onClick={() => setEpic(null)}>{t('All issues')}</button>
            {data.epics.map((e) => {
              const pct = e.childCount ? Math.round((e.childDone / e.childCount) * 100) : 0
              return (
                <button key={e.id} className={`epic-item ${epic === e.id ? 'on' : ''}`} onClick={() => setEpic(epic === e.id ? null : e.id)} onDoubleClick={() => open(e.key)} title={t('Double-click to open')}>
                  <span className="row" style={{ gap: 6 }}>
                    <span className="dot" style={{ background: e.type.color }} />
                    <span style={{ fontWeight: 500, flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{e.title}</span>
                  </span>
                  <span className="progress"><span className="p-done" style={{ width: `${pct}%` }} /></span>
                  <span className="faint small">{t('{done} of {total} done', { done: e.childDone, total: e.childCount })}{e.dueDate ? ` · ${shortDate(e.dueDate)}` : ''}</span>
                </button>
              )
            })}
            <button className={`epic-item ${epic === 'none' ? 'on' : ''}`} onClick={() => setEpic(epic === 'none' ? null : 'none')}>{t('Issues without epic')}</button>
          </aside>
        )}
        <div className="col" style={{ gap: 14, minWidth: 0 }}>
          {data.sprints.map((s) =>
            box(
              s.id,
              <>
                {s.name}
                {s.state === 'active' && <span className="badge badge-accent">{t('active')}</span>}
              </>,
              s.issues,
              <>
                {s.startAt && s.endAt && <span className="faint small">{shortDate(s.startAt)} – {shortDate(s.endAt)}{s.state === 'active' ? ` · ${t('{n} days left', { n: Math.max(0, daysUntil(s.endAt.slice(0, 10))) })}` : ''}</span>}
                {s.goal && <span className="faint small row" style={{ gap: 4 }} title={t('Sprint goal')}><Target size={12} /> {s.goal}</span>}
              </>,
              s,
            ),
          )}
          {box('backlog', t('Backlog'), data.backlog, null)}
          {data.sprints.length === 0 && data.backlog.length === 0 && <Empty title={t('Nothing planned yet')}>{t('Create issues, then plan sprints by dragging them in.')}</Empty>}
        </div>
      </div>
      {editSprint && <SprintDialog project={project} sprint={editSprint === 'new' ? null : editSprint} onClose={() => setEditSprint(null)} />}
      {starting && <StartSprint sprint={starting} onClose={() => setStarting(null)} />}
      {completing && <CompleteSprint project={project} sprint={completing} issues={completing.issues} onClose={() => setCompleting(null)} />}
      {deleting && (
        <Confirm
          title={t('Delete {name}?', { name: deleting.name })}
          body={<p>{t('Its issues go back to the backlog.')}</p>}
          confirmLabel={t('Delete sprint')}
          danger
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            await api.del(`/sprints/${deleting.id}`)
            qc.invalidateQueries({ queryKey })
            qc.invalidateQueries({ queryKey: ['project', project.key] })
          }}
        />
      )}
    </div>
  )
}

function InlineCreate({ project, sprintId, parentId }: { project: Project; sprintId: string | null; parentId?: string }) {
  const [open, setOpen] = useState(false)
  const [title, setTitle] = useState('')
  const types = project.types.filter((x) => x.level === 0)
  const [typeId, setTypeId] = useState(types[0]?.id)
  const qc = useQueryClient()
  const toast = useToast()
  if (!open)
    return (
      <button className="add-row" onClick={() => setOpen(true)}>
        <Plus size={14} /> {t('Create issue')}
      </button>
    )
  return (
    <form
      className="inline-create"
      onSubmit={async (e) => {
        e.preventDefault()
        if (!title.trim()) return
        try {
          await api.post('/issues', { projectKey: project.key, title, typeId, sprintId, ...(parentId ? { parentId } : {}) })
          setTitle('')
          qc.invalidateQueries({ queryKey: ['backlog', project.key] })
          refreshIssues(qc)
        } catch (err) {
          toast((err as Error).message)
        }
      }}
    >
      <select className="select input-sm" style={{ width: 'auto' }} value={typeId} onChange={(e) => setTypeId(e.target.value)} aria-label={t('Type')}>
        {types.map((x) => <option key={x.id} value={x.id}>{x.name}</option>)}
      </select>
      <input autoFocus value={title} onChange={(e) => setTitle(e.target.value)} placeholder={t('What needs to be done? Enter to create, Esc to stop')} onKeyDown={(e) => e.key === 'Escape' && setOpen(false)} onBlur={() => !title && setOpen(false)} />
    </form>
  )
}

function SprintDialog({ project, sprint, onClose }: { project: Project; sprint: Sprint | null; onClose: () => void }) {
  const qc = useQueryClient()
  const toast = useToast()
  const [name, setName] = useState(sprint?.name ?? '')
  const [goal, setGoal] = useState(sprint?.goal ?? '')
  const [start, setStart] = useState(sprint?.startAt?.slice(0, 10) ?? '')
  const [end, setEnd] = useState(sprint?.endAt?.slice(0, 10) ?? '')
  return (
    <Modal
      title={sprint ? t('Edit sprint') : t('Create sprint')}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button
            className="btn btn-primary"
            onClick={async () => {
              try {
                const body = { name, goal, startAt: start || null, endAt: end || null }
                if (sprint) await api.patch(`/sprints/${sprint.id}`, body)
                else await api.post(`/projects/${project.key}/sprints`, body)
                qc.invalidateQueries({ queryKey: ['backlog', project.key] })
                qc.invalidateQueries({ queryKey: ['project', project.key] })
                onClose()
              } catch (e) {
                toast((e as Error).message)
              }
            }}
          >{sprint ? t('Save') : t('Create sprint')}</button>
        </>
      }
    >
      <div className="field"><label htmlFor="sn">{t('Name')}</label><input id="sn" className="input" autoFocus value={name} placeholder={t('Leave empty for the next number')} onChange={(e) => setName(e.target.value)} /></div>
      <div className="field"><label htmlFor="sg">{t('Sprint goal')}</label><textarea id="sg" className="textarea" value={goal} onChange={(e) => setGoal(e.target.value)} placeholder={t('What should this sprint achieve?')} /></div>
      <div className="row">
        <div className="field" style={{ flex: 1 }}><label htmlFor="ss">{t('Start')}</label><input id="ss" type="date" className="input" value={start} onChange={(e) => setStart(e.target.value)} /></div>
        <div className="field" style={{ flex: 1 }}><label htmlFor="se">{t('End')}</label><input id="se" type="date" className="input" value={end} onChange={(e) => setEnd(e.target.value)} /></div>
      </div>
    </Modal>
  )
}

function StartSprint({ sprint, onClose }: { sprint: Sprint & { issues?: Issue[] }; onClose: () => void }) {
  const qc = useQueryClient()
  const toast = useToast()
  const today = isoDate(new Date())
  const [weeks, setWeeks] = useState(2)
  const [start, setStart] = useState(sprint.startAt?.slice(0, 10) ?? today)
  const endDefault = (s: string, w: number) => {
    const d = new Date(s + 'T00:00:00')
    d.setDate(d.getDate() + w * 7)
    return isoDate(d)
  }
  const [end, setEnd] = useState(sprint.endAt?.slice(0, 10) ?? endDefault(start, 2))
  const [goal, setGoal] = useState(sprint.goal ?? '')
  return (
    <Modal
      title={t('Start {name}', { name: sprint.name })}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button
            className="btn btn-primary"
            onClick={async () => {
              try {
                await api.post(`/sprints/${sprint.id}/start`, { startAt: start, endAt: end, goal })
                for (const k of ['backlog', 'board', 'project']) qc.invalidateQueries({ queryKey: [k] })
                toast(t('{name} started', { name: sprint.name }))
                onClose()
              } catch (e) {
                toast((e as Error).message)
              }
            }}
          ><Play size={14} /> {t('Start sprint')}</button>
        </>
      }
    >
      <p className="muted" style={{ marginTop: 0 }}>{t('{count} issues are planned for this sprint.', { count: sprint.issues?.length ?? 0 })}</p>
      <div className="field">
        <span className="field-label">{t('Duration')}</span>
        <div className="seg" role="group">
          {[1, 2, 3, 4].map((w) => (
            <button key={w} type="button" className={weeks === w ? 'active' : ''} onClick={() => { setWeeks(w); setEnd(endDefault(start, w)) }}>{t('{n} weeks', { n: w })}</button>
          ))}
        </div>
      </div>
      <div className="row">
        <div className="field" style={{ flex: 1 }}><label htmlFor="st1">{t('Start')}</label><input id="st1" type="date" className="input" value={start} onChange={(e) => { setStart(e.target.value); setEnd(endDefault(e.target.value, weeks)) }} /></div>
        <div className="field" style={{ flex: 1 }}><label htmlFor="st2">{t('End')}</label><input id="st2" type="date" className="input" value={end} onChange={(e) => setEnd(e.target.value)} /></div>
      </div>
      <div className="field"><label htmlFor="st3">{t('Sprint goal')}</label><textarea id="st3" className="textarea" value={goal} onChange={(e) => setGoal(e.target.value)} /></div>
    </Modal>
  )
}
