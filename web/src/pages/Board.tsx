import { t } from '../i18n'
import { Fragment, useMemo, useState } from 'react'
import { Link, useParams, useSearchParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Search, Rows3, ChevronDown, ChevronRight, Flag, Target, Kanban } from 'lucide-react'
import { api, qs, daysUntil, shortDate, type BoardData, type Issue, type Project } from '../api'
import { Avatar, Empty, ErrorBox, Loading, Modal, PopButton, Unassigned, useMe, useProject, useToast } from '../ui'
import { PriorityIcon, TypeIcon } from '../icons'
import IssueCard from '../components/IssueCard'
import { allowedStatuses } from '../components/fields'
import { useCreateIssue, useOpenIssue } from '../components/Layout'
import { refreshIssues } from '../components/IssueView'

type Lane = { id: string; title: React.ReactNode; issues: Issue[] }

// the quick filters every board starts with are stored in English; names people give their own
// filters stay as typed
const BUILT_IN_FILTERS: Record<string, () => string> = {
  'Recently updated': () => t('Recently updated'),
  Blocked: () => t('Blocked'),
}

export default function Board() {
  const { key } = useParams()
  const project = useProject(key)
  const [params, setParams] = useSearchParams()
  const boardId = params.get('board') ?? project.data?.boards[0]?.id
  const [quick, setQuick] = useState<string[]>([])
  const qFilters = useMemo(() => quick.map((q) => `(${q})`).join(' and '), [quick])
  const data = useQuery({
    queryKey: ['board', boardId, qFilters, params.get('sprint')],
    queryFn: () => api.get<BoardData>(`/boards/${boardId}${qs({ q: qFilters, sprint: params.get('sprint') })}`),
    enabled: !!boardId,
  })
  if (project.isLoading || (boardId && data.isLoading && !data.data)) return <Loading />
  if (project.error) return <div className="page"><ErrorBox error={project.error} /></div>
  if (data.error) return <div className="page"><ErrorBox error={data.error} /></div>
  if (!project.data || !data.data) return null
  return (
    <BoardView
      project={project.data}
      data={data.data}
      quick={quick}
      setQuick={setQuick}
      onBoard={(id) => {
        const p = new URLSearchParams(params)
        p.set('board', id)
        setParams(p)
      }}
      onSprint={(id) => {
        const p = new URLSearchParams(params)
        p.set('sprint', id)
        setParams(p)
      }}
    />
  )
}

function BoardView({ project, data, quick, setQuick, onBoard, onSprint }: { project: Project; data: BoardData; quick: string[]; setQuick: (q: string[]) => void; onBoard: (id: string) => void; onSprint: (id: string) => void }) {
  const qc = useQueryClient()
  const toast = useToast()
  const open = useOpenIssue()
  const create = useCreateIssue()
  const me = useMe().data?.user
  const cfg = data.board.config
  const [text, setText] = useState('')
  const [people, setPeople] = useState<string[]>([])
  const [lanes, setLanes] = useState<string>(cfg.swimlanes ?? 'none')
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set())
  const [drag, setDrag] = useState<{ id: string } | null>(null)
  const [over, setOver] = useState<{ col: string; lane: string; index: number } | null>(null)
  const [completing, setCompleting] = useState(false)
  const [adding, setAdding] = useState<string | null>(null)
  const queryKey = ['board', data.board.id]

  const assignees = useMemo(() => {
    const m = new Map<string, string>()
    data.issues.forEach((i) => i.assignee && m.set(i.assignee.id, i.assignee.name))
    return [...m.entries()]
  }, [data.issues])

  const issues = data.issues.filter((i) => {
    if (people.length && !people.includes(i.assignee?.id ?? 'none')) return false
    if (text && !`${i.key} ${i.title} ${i.labels.join(' ')}`.toLowerCase().includes(text.toLowerCase())) return false
    return true
  })

  const columnOf = (statusId: string) => cfg.columns.find((c) => c.statusIds.includes(statusId))

  const laneList: Lane[] = useMemo(() => {
    if (lanes === 'none') return [{ id: 'all', title: null, issues }]
    const groups = new Map<string, Lane>()
    const put = (id: string, title: React.ReactNode, i: Issue) => {
      if (!groups.has(id)) groups.set(id, { id, title, issues: [] })
      groups.get(id)!.issues.push(i)
    }
    for (const i of issues) {
      if (lanes === 'assignee') put(i.assignee?.id ?? 'none', i.assignee ? <><Avatar name={i.assignee.name} id={i.assignee.id} size={20} /> {i.assignee.name}</> : <><Unassigned size={20} /> {t('Unassigned')}</>, i)
      else if (lanes === 'epic') put(i.parent?.id ?? 'none', i.parent ? <><span className="dot" style={{ background: i.parent.color ?? 'var(--accent)' }} /> {i.parent.title} <span className="issue-key">{i.parent.key}</span></> : t('No epic'), i)
      else if (lanes === 'priority') put(i.priority, <PriorityIcon priority={i.priority} withLabel />, i)
      else if (lanes === 'type') put(i.type.id, <><TypeIcon icon={i.type.icon} color={i.type.color} /> {i.type.name}</>, i)
    }
    const list = [...groups.values()]
    if (lanes === 'priority') {
      const order = ['urgent', 'high', 'medium', 'low', 'none']
      list.sort((a, b) => order.indexOf(a.id) - order.indexOf(b.id))
    } else list.sort((a, b) => (a.id === 'none' ? 1 : b.id === 'none' ? -1 : 0))
    return list
  }, [issues, lanes])

  const move = async (issue: Issue, colId: string, laneId: string, index: number, colIssues: Issue[]) => {
    const col = cfg.columns.find((c) => c.id === colId)
    if (!col) return
    const allowed = allowedStatuses(project, issue.status.id).map((s) => s.id)
    const status = col.statusIds.includes(issue.status.id) ? issue.status.id : col.statusIds.find((s) => allowed.includes(s))
    if (!status) {
      toast(t('The workflow doesn’t allow moving {key} to {column}', { key: issue.key, column: col.name }))
      return
    }
    const others = colIssues.filter((i) => i.id !== issue.id)
    const after = others[index - 1]?.id ?? null
    const before = others[index]?.id ?? null
    const patch: Record<string, unknown> = { after, before: after ? null : before }
    if (status !== issue.status.id) patch.statusId = status
    // lanes by person / priority: dropping in another lane changes that too
    const lanePatch: Record<string, unknown> = {}
    if (lanes === 'assignee' && laneId !== (issue.assignee?.id ?? 'none')) lanePatch.assigneeId = laneId === 'none' ? null : laneId
    if (lanes === 'priority' && laneId !== issue.priority) lanePatch.priority = laneId
    // optimistic: put it there right away
    const target = project.statuses.find((s) => s.id === status)!
    const ranks = [others[index - 1]?.rank, others[index]?.rank]
    const rank = ranks[0] != null && ranks[1] != null ? (ranks[0] + ranks[1]) / 2 : ranks[0] != null ? ranks[0] + 1 : ranks[1] != null ? ranks[1] - 1 : issue.rank
    qc.setQueriesData<BoardData>({ queryKey }, (old) =>
      old ? { ...old, issues: old.issues.map((i) => (i.id === issue.id ? { ...i, status: { ...target }, rank, statusChangedAt: status !== issue.status.id ? new Date().toISOString() : i.statusChangedAt } : i)).sort((a, b) => a.rank - b.rank) } : old,
    )
    try {
      await api.post(`/issues/${issue.key}/move`, patch)
      if (Object.keys(lanePatch).length) await api.patch(`/issues/${issue.key}`, lanePatch)
    } catch (e) {
      toast((e as Error).message)
    }
    qc.invalidateQueries({ queryKey })
    refreshIssues(qc)
  }

  const sprint = data.sprint
  const sprintDays = sprint?.endAt ? daysUntil(sprint.endAt.slice(0, 10)) : null
  const doneCol = cfg.columns.length ? cfg.columns[cfg.columns.length - 1] : null

  return (
    <div className="board-wrap">
      <div className="board-head">
        <div className="crumbs"><Link to="/projects">{t('Projects')}</Link><span>/</span><Link to={`/projects/${project.key}`}>{project.name}</Link></div>
        <div className="page-head" style={{ marginBottom: 10 }}>
          <h1>{data.board.kind === 'scrum' ? sprint?.name ?? t('Active sprint') : data.board.name}</h1>
          {project.boards.length > 1 && (
            <PopButton className="btn btn-sm" label={<>{data.board.name} <ChevronDown size={14} /></>} width={220}>
              {(close) => project.boards.map((b) => <button key={b.id} className={`menu-item ${b.id === data.board.id ? 'selected' : ''}`} onClick={() => { close(); onBoard(b.id) }}>{b.name}</button>)}
            </PopButton>
          )}
          {data.activeSprints.length > 1 && (
            <PopButton className="btn btn-sm" label={<>{t('Sprint')} <ChevronDown size={14} /></>} width={220}>
              {(close) => data.activeSprints.map((s) => <button key={s.id} className={`menu-item ${s.id === sprint?.id ? 'selected' : ''}`} onClick={() => { close(); onSprint(s.id) }}>{s.name}</button>)}
            </PopButton>
          )}
          <span className="spacer" />
          {sprint && (
            <div className="sprint-banner">
              {sprint.goal && <span title={t('Sprint goal')} className="row" style={{ gap: 4 }}><Target size={14} /> {sprint.goal}</span>}
              {sprintDays != null && (
                <span className={`badge ${sprintDays < 0 ? 'badge-danger' : sprintDays <= 2 ? 'badge-warning' : ''}`}>
                  {sprintDays < 0 ? t('ended {n} days ago', { n: -sprintDays }) : sprintDays === 0 ? t('ends today') : t('{n} days left', { n: sprintDays })}
                </span>
              )}
              {sprint.endAt && <span className="faint">{shortDate(sprint.startAt)} – {shortDate(sprint.endAt)}</span>}
              {data.canEdit && <button className="btn" onClick={() => setCompleting(true)}><Flag size={14} /> {t('Complete sprint')}</button>}
            </div>
          )}
        </div>
        <div className="toolbar" style={{ marginBottom: 0 }}>
          <div className="row" style={{ position: 'relative' }}>
            <Search size={14} className="faint" style={{ position: 'absolute', left: 9 }} />
            <input className="input input-sm" style={{ width: 200, paddingLeft: 28 }} placeholder={t('Filter cards…')} value={text} onChange={(e) => setText(e.target.value)} aria-label={t('Filter cards')} />
          </div>
          {assignees.length > 0 && (
            <div className="avatar-filter" role="group" aria-label={t('Filter by person')}>
              {assignees.slice(0, 8).map(([id, name]) => (
                <button key={id} className={people.includes(id) ? 'on' : ''} title={name} aria-pressed={people.includes(id)} onClick={() => setPeople((p) => (p.includes(id) ? p.filter((x) => x !== id) : [...p, id]))}>
                  <Avatar name={name} id={id} size={28} />
                </button>
              ))}
            </div>
          )}
          <div className="quick-filters">
            {me && (
              <button className={`chip ${people.length === 1 && people[0] === me.id ? 'on' : ''}`} onClick={() => setPeople((p) => (p.length === 1 && p[0] === me.id ? [] : [me.id]))}>{t('Only my issues')}</button>
            )}
            {cfg.quickFilters.filter((f) => f.query !== 'assignee = me()').map((f) => (
              <button key={f.name} className={`chip ${quick.includes(f.query) ? 'on' : ''}`} aria-pressed={quick.includes(f.query)} onClick={() => setQuick(quick.includes(f.query) ? quick.filter((q) => q !== f.query) : [...quick, f.query])}>{BUILT_IN_FILTERS[f.name]?.() ?? f.name}</button>
            ))}
          </div>
          <span className="spacer" />
          <PopButton className="btn btn-sm" label={<><Rows3 size={14} /> {t('Swimlanes')}: {laneLabel(lanes)}</>} width={200} align="end">
            {(close) =>
              ['none', 'assignee', 'epic', 'priority', 'type'].map((l) => (
                <button key={l} className={`menu-item ${lanes === l ? 'selected' : ''}`} onClick={() => { close(); setLanes(l) }}>{laneLabel(l)}</button>
              ))
            }
          </PopButton>
        </div>
      </div>

      {data.board.kind === 'scrum' && !sprint ? (
        <Empty icon={<Kanban size={36} />} title={t('No sprint is running')}>
          <p>{t('Plan a sprint in the backlog and start it; its issues show up here.')}</p>
          <Link className="btn btn-primary" to={`/projects/${project.key}/backlog`}>{t('Go to the backlog')}</Link>
        </Empty>
      ) : (
        <div className={`board ${lanes !== 'none' ? 'has-lanes' : ''}`}>
          {laneList.map((lane) => {
            const isCollapsed = collapsed.has(lane.id)
            const cols = cfg.columns.map((col) => {
              const colIssues = lane.issues.filter((i) => columnOf(i.status.id)?.id === col.id).sort((a, b) => a.rank - b.rank)
              const total = issues.filter((i) => columnOf(i.status.id)?.id === col.id).length
              const overLimit = col.wipLimit != null && total > col.wipLimit
              const points = colIssues.reduce((s, i) => s + (i.storyPoints ?? 0), 0)
              const isOver = over?.col === col.id && over.lane === lane.id
              return (
                <div key={col.id} className={`column ${overLimit ? 'over' : ''}`}>
                  {(lane.id === 'all' || lane === laneList[0]) && (
                    <div className={`column-head ${overLimit ? 'over' : ''}`}>
                      <span>{col.name}</span>
                      <span className="count">{col.wipLimit != null ? `${total} / ${col.wipLimit}` : total}</span>
                      {overLimit && <span className="badge badge-danger" style={{ textTransform: 'none' }}>{t('over the limit')}</span>}
                      <span className="spacer" />
                      {points > 0 && <span className="faint" style={{ textTransform: 'none', fontWeight: 500 }}>{t('{n} pts', { n: points })}</span>}
                    </div>
                  )}
                  <div
                    className={`column-body ${isOver ? 'drop' : ''}`}
                    onDragOver={(e) => {
                      if (!drag) return
                      e.preventDefault()
                      const cards = [...e.currentTarget.querySelectorAll<HTMLElement>('.issue-card')]
                      let index = cards.length
                      for (let k = 0; k < cards.length; k++) {
                        const r = cards[k].getBoundingClientRect()
                        if (e.clientY < r.top + r.height / 2) {
                          index = k
                          break
                        }
                      }
                      if (!over || over.col !== col.id || over.lane !== lane.id || over.index !== index) setOver({ col: col.id, lane: lane.id, index })
                    }}
                    onDragLeave={(e) => {
                      if (!e.currentTarget.contains(e.relatedTarget as Node)) setOver(null)
                    }}
                    onDrop={(e) => {
                      e.preventDefault()
                      const issue = data.issues.find((i) => i.id === drag?.id)
                      const idx = over?.index ?? colIssues.length
                      setOver(null)
                      setDrag(null)
                      if (issue) {
                        // index among the cards shown, without the dragged one
                        const shown = colIssues.filter((i) => i.id !== issue.id)
                        const before = colIssues.slice(0, idx).filter((i) => i.id !== issue.id).length
                        void move(issue, col.id, lane.id, before, [...shown])
                      }
                    }}
                  >
                    {colIssues.map((i, k) => (
                      <Fragment key={i.id}>
                        {isOver && over.index === k && <div className="drop-line" />}
                        <IssueCard
                          issue={i}
                          fields={cfg.cardFields}
                          showEpic={lanes !== 'epic'}
                          onOpen={() => open(i.key)}
                          draggable={data.canEdit}
                          dragging={drag?.id === i.id}
                          onDragStart={(e) => {
                            e.dataTransfer.effectAllowed = 'move'
                            e.dataTransfer.setData('text/plain', i.key)
                            setDrag({ id: i.id })
                          }}
                          onDragEnd={() => {
                            setDrag(null)
                            setOver(null)
                          }}
                        />
                      </Fragment>
                    ))}
                    {isOver && over.index >= colIssues.length && <div className="drop-line" />}
                    {adding === `${lane.id}:${col.id}` && (
                      <QuickAdd
                        onCancel={() => setAdding(null)}
                        onCreate={async (title) => {
                          try {
                            const body: Record<string, unknown> = { projectKey: project.key, title }
                            if (sprint) body.sprintId = sprint.id
                            if (lanes === 'assignee' && lane.id !== 'none') body.assigneeId = lane.id
                            if (lanes === 'epic' && lane.id !== 'none') body.parentId = lane.id
                            const created = await api.post<Issue>('/issues', body)
                            if (!col.statusIds.includes(created.status.id)) await api.patch(`/issues/${created.key}`, { statusId: col.statusIds[0] })
                            qc.invalidateQueries({ queryKey })
                          } catch (e) {
                            toast((e as Error).message)
                          }
                        }}
                      />
                    )}
                  </div>
                  {data.canEdit && col !== doneCol && adding !== `${lane.id}:${col.id}` && (lane.id === 'all' || !isCollapsed) && (
                    <button className="btn btn-ghost btn-sm column-add" onClick={() => setAdding(`${lane.id}:${col.id}`)}>
                      <Plus size={14} /> {t('Create')}
                    </button>
                  )}
                </div>
              )
            })
            if (lane.id === 'all') return <Fragment key="all">{cols}</Fragment>
            return (
              <div key={lane.id} className="lane">
                <button className="lane-head" onClick={() => setCollapsed((s) => { const n = new Set(s); if (n.has(lane.id)) n.delete(lane.id); else n.add(lane.id); return n })} aria-expanded={!isCollapsed}>
                  {isCollapsed ? <ChevronRight size={15} /> : <ChevronDown size={15} />}
                  {lane.title}
                  <span className="faint small">{t('{count} issues', { count: lane.issues.length })}</span>
                </button>
                {!isCollapsed && <div className="lane-cols">{cols}</div>}
              </div>
            )
          })}
        </div>
      )}
      {data.canEdit && lanes === 'none' && issues.length === 0 && sprint === null && data.board.kind === 'kanban' && (
        <div style={{ padding: '0 20px' }}><button className="btn" onClick={() => create({ projectKey: project.key })}><Plus size={14} /> {t('Create issue')}</button></div>
      )}
      {completing && sprint && <CompleteSprint project={project} sprint={sprint} issues={data.issues} onClose={() => setCompleting(false)} />}
    </div>
  )
}

function laneLabel(l: string) {
  switch (l) {
    case 'assignee':
      return t('People')
    case 'epic':
      return t('Epics')
    case 'priority':
      return t('Priority')
    case 'type':
      return t('Type')
    default:
      return t('None')
  }
}

function QuickAdd({ onCreate, onCancel }: { onCreate: (title: string) => Promise<void>; onCancel: () => void }) {
  const [v, setV] = useState('')
  return (
    <div className="issue-card" style={{ cursor: 'default' }}>
      <textarea
        className="input"
        style={{ height: 60, padding: 6, resize: 'none' }}
        autoFocus
        placeholder={t('What needs to be done?')}
        value={v}
        onChange={(e) => setV(e.target.value)}
        onKeyDown={async (e) => {
          if (e.key === 'Escape') onCancel()
          if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault()
            if (v.trim()) {
              await onCreate(v.trim())
              setV('')
            }
          }
        }}
        onBlur={() => !v.trim() && onCancel()}
      />
      <div className="faint small">{t('Enter to create, Esc to cancel')}</div>
    </div>
  )
}

export function CompleteSprint({ project, sprint, issues, onClose }: { project: Project; sprint: { id: string; name: string }; issues: Issue[]; onClose: () => void }) {
  const qc = useQueryClient()
  const toast = useToast()
  const me = useMe().data
  const done = issues.filter((i) => i.status.category === 'done')
  const open = issues.filter((i) => i.status.category !== 'done')
  const planned = project.sprints.filter((s) => s.state === 'future')
  const [target, setTarget] = useState<string>(planned[0]?.id ?? 'new')
  const [review, setReview] = useState(!!(me?.wikicraft?.connected && project.settings.wikiSpace))
  const [busy, setBusy] = useState(false)
  return (
    <Modal
      title={t('Complete {name}', { name: sprint.name })}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button
            className="btn btn-primary"
            disabled={busy}
            onClick={async () => {
              setBusy(true)
              try {
                const r = await api.post<{ moved: number }>(`/sprints/${sprint.id}/complete`, { moveTo: target })
                for (const k of ['board', 'backlog', 'project', 'report']) qc.invalidateQueries({ queryKey: [k] })
                refreshIssues(qc)
                if (review) {
                  try {
                    const p = await api.post<{ url: string }>(`/sprints/${sprint.id}/wiki-page`, {})
                    toast(t('Sprint completed; review page written'), { label: t('Open [[action]]'), run: () => window.open(p.url, '_blank', 'noopener') })
                  } catch (e) {
                    toast(t('Sprint completed, but the wiki page failed: {error}', { error: (e as Error).message }))
                  }
                } else toast(t('Sprint completed; {n} issues moved', { n: r.moved }))
                onClose()
              } catch (e) {
                toast((e as Error).message)
              } finally {
                setBusy(false)
              }
            }}
          >{t('Complete sprint')}</button>
        </>
      }
    >
      <p>{t('{done} issues are done and {open} are not.', { done: done.length, open: open.length })}</p>
      {open.length > 0 && (
        <div className="field">
          <label htmlFor="cs-target">{t('Move the unfinished issues to')}</label>
          <select id="cs-target" className="select" value={target} onChange={(e) => setTarget(e.target.value)}>
            {planned.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
            <option value="new">{t('A new sprint')}</option>
            <option value="backlog">{t('The backlog')}</option>
          </select>
        </div>
      )}
      {me?.wikicraft?.connected && (
        <label className="row small" style={{ marginTop: 8 }}>
          <input type="checkbox" checked={review} onChange={(e) => setReview(e.target.checked)} disabled={!project.settings.wikiSpace} />
          {project.settings.wikiSpace ? t('Write a sprint review page in the wiki (space {space})', { space: project.settings.wikiSpace }) : t('Set the project’s wiki space in its settings to write a sprint review page')}
        </label>
      )}
      <p className="faint small">{t('The sprint’s figures (committed, completed, scope changes) are kept for the velocity report.')}</p>
    </Modal>
  )
}
