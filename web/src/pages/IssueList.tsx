// The issue list: a TQL query shown as a table with chosen columns, sorting, grouping, inline
import { useTransition } from '../components/Transition'
// editing and bulk changes. Project-scoped at /projects/:key/list, workspace-wide at /search
// (with saved filters).
import { t } from '../i18n'
import { Fragment, useEffect, useMemo, useState } from 'react'
import { Link, useParams, useSearchParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Columns3, Download, Rows3, Save, Star, Trash, ArrowUp, ArrowDown, Ellipsis, Share2, ListFilter } from 'lucide-react'
import { api, qs, shortDate, duration, ago, type Issue, type Project, type SavedView, type CustomField } from '../api'
import { Avatar, Confirm, Empty, ErrorBox, Loading, Modal, PopButton, useMe, usePeople, useProject, useProjects, useToast } from '../ui'
import { PriorityIcon, TypeIcon, priorityLabel } from '../icons'
import QueryBar from '../components/QueryBar'
import { OptionChip, PersonPicker, PriorityPicker, StatusPicker, DueText } from '../components/fields'
import { useOpenIssue } from '../components/Layout'
import { refreshIssues } from '../components/IssueView'

type ColId = string
const BASE_COLUMNS: { id: ColId; label: () => string; sort?: string }[] = [
  { id: 'type', label: () => t('Type'), sort: 'type' },
  { id: 'key', label: () => t('Key'), sort: 'key' },
  { id: 'title', label: () => t('Title'), sort: 'title' },
  { id: 'status', label: () => t('Status'), sort: 'status' },
  { id: 'priority', label: () => t('Priority'), sort: 'priority' },
  { id: 'assignee', label: () => t('Assignee'), sort: 'assignee' },
  { id: 'reporter', label: () => t('Reporter'), sort: 'reporter' },
  { id: 'project', label: () => t('Project'), sort: 'project' },
  { id: 'parent', label: () => t('Epic / parent') },
  { id: 'sprint', label: () => t('Sprint') },
  { id: 'version', label: () => t('Release') },
  { id: 'labels', label: () => t('Labels') },
  { id: 'points', label: () => t('Points'), sort: 'points' },
  { id: 'estimate', label: () => t('Estimate'), sort: 'estimate' },
  { id: 'spent', label: () => t('Spent'), sort: 'spent' },
  { id: 'start', label: () => t('Start'), sort: 'start' },
  { id: 'due', label: () => t('Due'), sort: 'due' },
  { id: 'created', label: () => t('Created'), sort: 'created' },
  { id: 'updated', label: () => t('Updated'), sort: 'updated' },
  { id: 'resolved', label: () => t('Resolved'), sort: 'resolved' },
]
const DEFAULT_COLUMNS = ['type', 'key', 'title', 'status', 'priority', 'assignee', 'due', 'updated']
const GROUPS = ['none', 'status', 'assignee', 'priority', 'type', 'sprint', 'parent', 'project'] as const
type Group = (typeof GROUPS)[number]

const groupLabel = (g: Group) =>
  ({ none: t('No grouping'), status: t('Status'), assignee: t('Assignee'), priority: t('Priority'), type: t('Type'), sprint: t('Sprint'), parent: t('Epic'), project: t('Project') })[g]

function hasOrder(q: string) {
  return /\border\s+by\b/i.test(q)
}

export default function IssueList() {
  const { key } = useParams()
  const [params, setParams] = useSearchParams()
  const project = useProject(key)
  const views = useQuery({ queryKey: ['views'], queryFn: () => api.get<SavedView[]>('/views'), enabled: !key })
  const viewId = params.get('view')
  const view = views.data?.find((v) => v.id === viewId)
  const defaultQuery = key ? 'category != done' : 'assignee = me() and category != done'
  const query = params.get('q') ?? view?.query ?? defaultQuery
  const [columns, setColumns] = useState<string[]>(() => {
    try {
      return JSON.parse(localStorage.getItem(`tc-cols-${key ?? 'all'}`) ?? '') as string[]
    } catch {
      return key ? DEFAULT_COLUMNS : [...DEFAULT_COLUMNS.slice(0, 3), 'project', ...DEFAULT_COLUMNS.slice(3)]
    }
  })
  const [group, setGroup] = useState<Group>('none')
  const [sort, setSort] = useState<{ field: string; desc: boolean } | null>(null)
  const [limit, setLimit] = useState(200)
  useEffect(() => {
    if (view?.config?.columns) setColumns(view.config.columns)
    if (view?.config?.group) setGroup(view.config.group)
  }, [view?.id]) // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    try {
      localStorage.setItem(`tc-cols-${key ?? 'all'}`, JSON.stringify(columns))
    } catch {
      /* ignore */
    }
  }, [columns, key])
  const effective = sort && !hasOrder(query) ? `${query} order by ${sort.field} ${sort.desc ? 'desc' : 'asc'}` : query
  const result = useQuery({
    queryKey: ['search', key ?? '', effective, limit],
    queryFn: () => api.get<{ total: number; issues: Issue[] }>(`/search${qs({ q: effective, project: key, limit, order: key ? 'rank' : undefined })}`),
    placeholderData: (prev) => prev,
  })
  const setQuery = (q: string) => {
    const p = new URLSearchParams(params)
    p.set('q', q)
    setParams(p)
  }
  if (key && project.isLoading) return <Loading />
  if (key && !project.data) return <div className="page"><ErrorBox error={project.error ?? t('Not found')} /></div>
  const fields: CustomField[] = project.data?.fields ?? []
  return (
    <div className="page-wide">
      {project.data && <div className="crumbs"><Link to="/projects">{t('Projects')}</Link><span>/</span><Link to={`/projects/${project.data.key}`}>{project.data.name}</Link></div>}
      <div className="page-head">
        <h1>{view ? view.name : key ? t('Issues') : t('Filters')}</h1>
        {!key && <ViewsMenu views={views.data ?? []} current={view} />}
        <span className="spacer" />
        <span className="faint small">{result.data ? t('{count} issues', { count: result.data.total }) : ''}</span>
      </div>
      <div className="toolbar">
        <QueryBar value={query} onChange={setQuery} />
      </div>
      <div className="toolbar">
        <QuickFilters query={query} setQuery={setQuery} project={project.data} />
        <span className="spacer" />
        <PopButton className="btn btn-sm" label={<><Rows3 size={14} /> {groupLabel(group)}</>} width={200} align="end">
          {(close) => GROUPS.filter((g) => key ? g !== 'project' : true).map((g) => <button key={g} className={`menu-item ${g === group ? 'selected' : ''}`} onClick={() => { close(); setGroup(g) }}>{groupLabel(g)}</button>)}
        </PopButton>
        <PopButton className="btn btn-sm" label={<><Columns3 size={14} /> {t('Columns')}</>} width={240} align="end">
          {() => (
            <div className="choices-list" style={{ maxHeight: 380, overflowY: 'auto' }}>
              {[...BASE_COLUMNS.map((c) => ({ id: c.id, label: c.label() })), ...fields.map((f) => ({ id: `cf:${f.id}`, label: f.name }))].map((c) => (
                <label key={c.id} className="menu-item">
                  <input type="checkbox" checked={columns.includes(c.id)} onChange={(e) => setColumns(e.target.checked ? [...columns, c.id] : columns.filter((x) => x !== c.id))} />
                  {c.label}
                </label>
              ))}
            </div>
          )}
        </PopButton>
        <a className="btn btn-sm" href={`/api/search.csv${qs({ q: effective, project: key })}`} download><Download size={14} /> {t('CSV')}</a>
        <SaveView query={query} columns={columns} group={group} projectKey={key} current={view} />
      </div>
      {result.error && <ErrorBox error={result.error} />}
      {result.isLoading && <Loading />}
      {result.data && (
        <IssueTable
          issues={result.data.issues}
          columns={columns}
          fields={fields}
          group={group}
          sort={sort}
          canSort={!hasOrder(query)}
          onSort={(f) => setSort((s) => (s?.field === f ? (s.desc ? null : { field: f, desc: true }) : { field: f, desc: false }))}
        />
      )}
      {result.data && result.data.total > result.data.issues.length && (
        <div style={{ textAlign: 'center', marginTop: 12 }}>
          <button className="btn" onClick={() => setLimit((l) => l + 200)}>{t('Show more ({shown} of {total})', { shown: result.data.issues.length, total: result.data.total })}</button>
        </div>
      )}
    </div>
  )
}

/** One-click filters that add or remove a clause. */
function QuickFilters({ query, setQuery, project }: { query: string; setQuery: (q: string) => void; project?: Project }) {
  const clauses = [
    { label: t('Assigned to me'), q: 'assignee = me()' },
    { label: t('Unassigned'), q: 'assignee is empty' },
    { label: t('Open'), q: 'category != done' },
    { label: t('Overdue'), q: 'due < today() and category != done' },
    { label: t('Updated this week'), q: 'updated >= startOfWeek()' },
    { label: t('Blocked'), q: 'blocked = true' },
    ...(project?.kind === 'scrum' ? [{ label: t('Current sprint'), q: 'sprint in openSprints()' }] : []),
  ]
  const [base, order] = (() => {
    const m = /\s+order\s+by\s+.*$/i.exec(query)
    return m ? [query.slice(0, m.index), m[0]] : [query, '']
  })()
  const has = (c: string) => base.includes(c)
  const toggle = (c: string) => {
    let next: string
    if (has(c)) {
      next = base.replace(` and ${c}`, '').replace(`${c} and `, '').replace(c, '').trim()
    } else next = base.trim() ? `${base.trim()} and ${c}` : c
    setQuery(next + order)
  }
  return (
    <div className="quick-filters">
      <ListFilter size={14} className="faint" />
      {clauses.map((c) => (
        <button key={c.q} className={`chip ${has(c.q) ? 'on' : ''}`} aria-pressed={has(c.q)} onClick={() => toggle(c.q)}>{c.label}</button>
      ))}
    </div>
  )
}

function ViewsMenu({ views, current }: { views: SavedView[]; current?: SavedView }) {
  const qc = useQueryClient()
  const [, setParams] = useSearchParams()
  const [del, setDel] = useState<SavedView | null>(null)
  return (
    <>
      <PopButton className="btn btn-sm" label={<><Star size={14} /> {t('Saved filters')}</>} width={320}>
        {(close) => (
          <div>
            {views.length === 0 && <div className="menu-empty">{t('Save a query to find it here and in the sidebar.')}</div>}
            {views.map((v) => (
              <div key={v.id} className="row" style={{ gap: 2 }}>
                <button className={`menu-item ${current?.id === v.id ? 'selected' : ''}`} style={{ flex: 1 }} onClick={() => { close(); setParams({ view: v.id }) }}>
                  {v.shared && <Share2 size={13} className="faint" />}
                  <span style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{v.name}</span>
                  {!v.mine && <span className="mi-desc">{v.ownerName}</span>}
                </button>
                <button className={`icon-btn icon-btn-sm ${v.starred ? 'active' : ''}`} aria-label={v.starred ? t('Unstar') : t('Star')} onClick={async () => { await api.put(`/views/${v.id}/star`, { starred: !v.starred }); qc.invalidateQueries({ queryKey: ['views'] }) }}>
                  <Star size={13} fill={v.starred ? 'currentColor' : 'none'} />
                </button>
                {v.mine && <button className="icon-btn icon-btn-sm" aria-label={t('Delete')} onClick={() => { close(); setDel(v) }}><Trash size={13} /></button>}
              </div>
            ))}
          </div>
        )}
      </PopButton>
      {del && (
        <Confirm title={t('Delete “{name}”?', { name: del.name })} body={<p>{t('The saved filter is removed for everyone it was shared with.')}</p>} confirmLabel={t('Delete')} danger onClose={() => setDel(null)} onConfirm={async () => { await api.del(`/views/${del.id}`); qc.invalidateQueries({ queryKey: ['views'] }) }} />
      )}
    </>
  )
}

function SaveView({ query, columns, group, projectKey, current }: { query: string; columns: string[]; group: Group; projectKey?: string; current?: SavedView }) {
  const [open, setOpen] = useState(false)
  const [name, setName] = useState('')
  const [shared, setShared] = useState(false)
  const qc = useQueryClient()
  const toast = useToast()
  const [, setParams] = useSearchParams()
  const me = useMe().data?.user
  return (
    <>
      {current?.mine ? (
        <PopButton className="btn btn-sm" label={<><Save size={14} /> {t('Save')}</>} align="end" width={220}>
          {(close) => (
            <>
              <button className="menu-item" onClick={async () => { close(); await api.patch(`/views/${current.id}`, { query, config: { columns, group } }); qc.invalidateQueries({ queryKey: ['views'] }); toast(t('Saved')) }}>{t('Update “{name}”', { name: current.name })}</button>
              <button className="menu-item" onClick={() => { close(); setOpen(true) }}>{t('Save as new…')}</button>
            </>
          )}
        </PopButton>
      ) : (
        <button className="btn btn-sm" onClick={() => setOpen(true)}><Save size={14} /> {t('Save filter')}</button>
      )}
      {open && (
        <Modal
          title={t('Save filter')}
          onClose={() => setOpen(false)}
          width={460}
          footer={
            <>
              <button className="btn" onClick={() => setOpen(false)}>{t('Cancel')}</button>
              <button
                className="btn btn-primary"
                disabled={!name.trim()}
                onClick={async () => {
                  try {
                    const r = await api.post<{ id: string }>('/views', { name, query, shared, projectKey, config: { columns, group } })
                    qc.invalidateQueries({ queryKey: ['views'] })
                    setOpen(false)
                    if (!projectKey) setParams({ view: r.id })
                    toast(t('Filter saved'))
                  } catch (e) {
                    toast((e as Error).message)
                  }
                }}
              >{t('Save')}</button>
            </>
          }
        >
          <div className="field"><label htmlFor="vn">{t('Name')}</label><input id="vn" className="input" autoFocus value={name} onChange={(e) => setName(e.target.value)} /></div>
          <div className="field"><span className="field-label">{t('Query')}</span><code className="small" style={{ wordBreak: 'break-all' }}>{query}</code></div>
          {me?.role !== 'guest' && <label className="row small"><input type="checkbox" checked={shared} onChange={(e) => setShared(e.target.checked)} /> {t('Share with everyone who can see the issues')}</label>}
        </Modal>
      )}
    </>
  )
}

function groupKey(i: Issue, g: Group): [string, React.ReactNode] {
  switch (g) {
    case 'status':
      return [i.status.name, <><span className={`lozenge lozenge-${i.status.category}`}>{i.status.name}</span></>]
    case 'assignee':
      return [i.assignee?.id ?? '~', i.assignee ? <><Avatar name={i.assignee.name} id={i.assignee.id} size={20} /> {i.assignee.name}</> : t('Unassigned')]
    case 'priority':
      return [String(['urgent', 'high', 'medium', 'low', 'none'].indexOf(i.priority)), <PriorityIcon priority={i.priority} withLabel />]
    case 'type':
      return [i.type.name, <><TypeIcon icon={i.type.icon} color={i.type.color} /> {i.type.name}</>]
    case 'sprint':
      return [i.sprint?.name ?? '~', i.sprint?.name ?? t('Backlog')]
    case 'parent':
      return [i.parent?.key ?? '~', i.parent ? <><span className="issue-key">{i.parent.key}</span> {i.parent.title}</> : t('No epic')]
    case 'project':
      return [i.projectKey, i.projectName]
    default:
      return ['', null]
  }
}

function IssueTable({ issues, columns, fields, group, sort, canSort, onSort }: { issues: Issue[]; columns: string[]; fields: CustomField[]; group: Group; sort: { field: string; desc: boolean } | null; canSort: boolean; onSort: (f: string) => void }) {
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const open = useOpenIssue()
  useEffect(() => setSelected((s) => new Set([...s].filter((id) => issues.some((i) => i.id === id)))), [issues])
  const groups = useMemo(() => {
    if (group === 'none') return [{ key: '', title: null as React.ReactNode, items: issues }]
    const m = new Map<string, { key: string; title: React.ReactNode; items: Issue[] }>()
    for (const i of issues) {
      const [k, title] = groupKey(i, group)
      if (!m.has(k)) m.set(k, { key: k, title, items: [] })
      m.get(k)!.items.push(i)
    }
    return [...m.values()].sort((a, b) => a.key.localeCompare(b.key))
  }, [issues, group])
  if (issues.length === 0) return <Empty icon={<ListFilter size={32} />} title={t('No issues match')}>{t('Change the query or remove a filter.')}</Empty>
  const cols = columns.filter((c) => BASE_COLUMNS.some((b) => b.id === c) || (c.startsWith('cf:') && fields.some((f) => `cf:${f.id}` === c)))
  const all = selected.size === issues.length
  return (
    <>
      <div style={{ overflowX: 'auto', border: '1px solid var(--border)', borderRadius: 10 }}>
        <table className="grid-table">
          <thead>
            <tr>
              <th style={{ width: 32 }}>
                <input type="checkbox" aria-label={t('Select all')} checked={all} onChange={() => setSelected(all ? new Set() : new Set(issues.map((i) => i.id)))} />
              </th>
              {cols.map((c) => {
                const base = BASE_COLUMNS.find((b) => b.id === c)
                const label = base ? base.label() : fields.find((f) => `cf:${f.id}` === c)?.name
                const sortable = canSort && !!base?.sort
                const sorted = sort?.field === base?.sort
                return (
                  <th key={c} className={sorted ? 'sorted' : ''} onClick={() => sortable && onSort(base!.sort!)} aria-sort={sorted ? (sort!.desc ? 'descending' : 'ascending') : undefined}>
                    <span className="row" style={{ gap: 4 }}>
                      {label}
                      {sorted && (sort!.desc ? <ArrowDown size={12} /> : <ArrowUp size={12} />)}
                    </span>
                  </th>
                )
              })}
            </tr>
          </thead>
          <tbody>
            {groups.map((g) => (
              <Fragment key={g.key}>
                {group !== 'none' && (
                  <tr className="group-head">
                    <td colSpan={cols.length + 1}>
                      <span className="row" style={{ gap: 8 }}>{g.title} <span className="faint small">{g.items.length}</span></span>
                    </td>
                  </tr>
                )}
                {g.items.map((i) => (
                  <tr key={i.id} className={selected.has(i.id) ? 'selected' : ''}>
                    <td>
                      <input type="checkbox" aria-label={t('Select {key}', { key: i.key })} checked={selected.has(i.id)} onChange={() => setSelected((s) => { const n = new Set(s); if (n.has(i.id)) n.delete(i.id); else n.add(i.id); return n })} />
                    </td>
                    {cols.map((c) => <Cell key={c} col={c} issue={i} fields={fields} onOpen={() => open(i.key)} />)}
                  </tr>
                ))}
              </Fragment>
            ))}
          </tbody>
        </table>
      </div>
      {selected.size > 0 && <BulkBar ids={[...selected]} issues={issues.filter((i) => selected.has(i.id))} onDone={() => setSelected(new Set())} />}
    </>
  )
}

function useSave() {
  const qc = useQueryClient()
  const toast = useToast()
  return async (key: string, patch: Record<string, unknown>) => {
    try {
      await api.patch(`/issues/${key}`, patch)
      refreshIssues(qc)
      qc.invalidateQueries({ queryKey: ['issue', key] })
    } catch (e) {
      toast((e as Error).message)
    }
  }
}

function StatusCell({ issue }: { issue: Issue }) {
  const project = useProject(issue.projectKey).data
  const save = useSave()
  if (!project) return <span className={`lozenge lozenge-${issue.status.category}`}>{issue.status.name}</span>
  return <StatusPicker compact project={project} status={issue.status} disabled={project.role === 'viewer'} onChange={(id) => save(issue.key, { statusId: id })} />
}

function Cell({ col, issue: i, fields, onOpen }: { col: string; issue: Issue; fields: CustomField[]; onOpen: () => void }) {
  const save = useSave()
  const people = usePeople()
  const done = i.status.category === 'done'
  switch (col) {
    case 'type':
      return <td style={{ width: 28 }}><TypeIcon icon={i.type.icon} color={i.type.color} title={i.type.name} /></td>
    case 'key':
      return <td><a className="issue-key" href={`/issues/${i.key}`} onClick={(e) => { e.preventDefault(); onOpen() }} style={{ textDecoration: done ? 'line-through' : undefined }}>{i.key}</a></td>
    case 'title':
      return (
        <td style={{ minWidth: 260, maxWidth: 520 }}>
          <a href={`/issues/${i.key}`} onClick={(e) => { e.preventDefault(); onOpen() }} style={{ color: 'var(--text)' }}>{i.title}</a>
          {i.childCount > 0 && <span className="faint small"> · {i.childDone}/{i.childCount}</span>}
        </td>
      )
    case 'status':
      return <td><StatusCell issue={i} /></td>
    case 'priority':
      return <td><PriorityPicker compact value={i.priority} onChange={(p) => save(i.key, { priority: p })} /></td>
    case 'assignee':
      return (
        <td>
          <PersonPicker className="cell-btn" value={i.assignee} onChange={(id) => save(i.key, { assigneeId: id })} />
        </td>
      )
    case 'reporter':
      return <td>{i.reporter ? <span className="row" style={{ gap: 6 }}><Avatar name={i.reporter.name} id={i.reporter.id} size={20} />{i.reporter.name}</span> : ''}</td>
    case 'project':
      return <td><Link to={`/projects/${i.projectKey}`}>{i.projectName}</Link></td>
    case 'parent':
      return <td>{i.parent ? <span className="row" style={{ gap: 6 }}><span className="dot" style={{ background: i.parent.color ?? 'var(--accent)' }} />{i.parent.title}</span> : ''}</td>
    case 'sprint':
      return <td>{i.sprint?.name ?? ''}</td>
    case 'version':
      return <td>{i.version?.name ?? ''}</td>
    case 'labels':
      return <td><span className="row" style={{ gap: 4 }}>{i.labels.map((l) => <span key={l} className="label-chip">{l}</span>)}</span></td>
    case 'points':
      return <td>{i.storyPoints ?? ''}</td>
    case 'estimate':
      return <td>{i.estimateMinutes != null ? duration(i.estimateMinutes) : ''}</td>
    case 'spent':
      return <td>{i.spentMinutes ? duration(i.spentMinutes) : ''}</td>
    case 'start':
      return <td>{shortDate(i.startDate)}</td>
    case 'due':
      return <td>{i.dueDate ? <DueText date={i.dueDate} done={done} /> : ''}</td>
    case 'created':
      return <td title={i.createdAt}>{shortDate(i.createdAt)}</td>
    case 'updated':
      return <td title={i.updatedAt}>{ago(i.updatedAt)}</td>
    case 'resolved':
      return <td>{shortDate(i.resolvedAt)}</td>
  }
  if (col.startsWith('cf:')) {
    const f = fields.find((x) => `cf:${x.id}` === col)
    const v = i.custom?.[col.slice(3)]
    if (!f || v == null) return <td />
    if (f.kind === 'select') {
      const o = f.options.find((x) => x.id === v)
      return <td>{o && <OptionChip label={o.label} color={o.color} />}</td>
    }
    if (f.kind === 'multiselect') return <td>{f.options.filter((o) => (v as string[]).includes(o.id)).map((o) => <OptionChip key={o.id} label={o.label} color={o.color} />)}</td>
    if (f.kind === 'user') return <td>{people.data?.find((p) => p.id === v)?.name ?? ''}</td>
    if (f.kind === 'checkbox') return <td>{v ? '✓' : ''}</td>
    if (f.kind === 'date') return <td>{shortDate(v)}</td>
    if (f.kind === 'number') return <td style={{ textAlign: 'end' }}>{Number(v).toLocaleString()}</td>
    return <td>{String(v)}</td>
  }
  return <td />
}

function BulkBar({ ids, issues, onDone }: { ids: string[]; issues: Issue[]; onDone: () => void }) {
  const qc = useQueryClient()
  const toast = useToast()
  const [del, setDel] = useState(false)
  const projects = useProjects()
  const sameProject = new Set(issues.map((i) => i.projectKey)).size === 1 ? issues[0]?.projectKey : undefined
  const project = useProject(sameProject).data
  // the target status's rules may ask for fields, a comment or a resolution (for all of them)
  const transition = useTransition()
  const run = async (body: Record<string, unknown>) => {
    const r = await api.post<{ updated: number; errors: { error: string }[] }>('/issues/bulk', { ids, ...body })
    refreshIssues(qc)
    toast(r.errors.length ? t('{n} changed, {m} failed: {error}', { n: r.updated, m: r.errors.length, error: r.errors[0].error }) : t('{n} issues changed', { n: r.updated }))
    onDone()
  }
  void projects
  return (
    <div className="bulk-bar" role="toolbar" aria-label={t('Change selected issues')}>
      <strong>{t('{n} selected', { n: ids.length })}</strong>
      {project && (
        <PopButton className="btn btn-sm" label={t('Status')} width={220}>
          {(close) => project.statuses.map((s) => <button key={s.id} className="menu-item" onClick={async () => { close(); const extra = await transition(null, project, s.id); if (extra) void run({ patch: { statusId: s.id, ...extra } }) }}><span className={`lozenge lozenge-${s.category}`}>{s.name}</span></button>)}
        </PopButton>
      )}
      <PersonPicker className="btn btn-sm" value={null} label={t('Assign')} onChange={(id) => run({ patch: { assigneeId: id } })} />
      <PopButton className="btn btn-sm" label={t('Priority')} width={180}>
        {(close) => (['urgent', 'high', 'medium', 'low', 'none'] as const).map((p) => <button key={p} className="menu-item" onClick={() => { close(); void run({ patch: { priority: p } }) }}><PriorityIcon priority={p} withLabel /> <span className="sr-only">{priorityLabel(p)}</span></button>)}
      </PopButton>
      {project?.kind === 'scrum' && (
        <PopButton className="btn btn-sm" label={t('Sprint')} width={220}>
          {(close) => (
            <>
              {project.sprints.map((s) => <button key={s.id} className="menu-item" onClick={() => { close(); void run({ patch: { sprintId: s.id } }) }}>{s.name}</button>)}
              <button className="menu-item" onClick={() => { close(); void run({ patch: { sprintId: null } }) }}>{t('Backlog')}</button>
            </>
          )}
        </PopButton>
      )}
      <PopButton className="btn btn-sm" label={<Ellipsis size={14} />} title={t('More')} width={200}>
        {(close) => (
          <>
            <button className="menu-item danger" onClick={() => { close(); setDel(true) }}><Trash size={14} /> {t('Delete')}</button>
          </>
        )}
      </PopButton>
      <button className="btn btn-sm" onClick={onDone}>{t('Clear')}</button>
      {del && <Confirm title={t('Delete {n} issues?', { n: ids.length })} body={<p>{t('They move to their projects’ trash, where they can be restored.')}</p>} confirmLabel={t('Delete')} danger onClose={() => setDel(false)} onConfirm={() => run({ action: 'delete' })} />}
    </div>
  )
}
