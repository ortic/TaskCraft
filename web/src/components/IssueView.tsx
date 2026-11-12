// An issue in full: title, description, children, links, wiki pages, files, comments, history,
// time logs, and every field on the side. Used in the side panel and on /issues/:key.
import { t } from '../i18n'
import { useEffect, useRef, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Link2, Paperclip, Eye, EyeOff, Ellipsis, Heart, Copy, Trash, ExternalLink, Plus, X, Ban, BookOpen, Clock, CornerDownRight, MessageSquare, Activity, Timer, FileText, Check,
} from 'lucide-react'
import {
  api, ago, dateTime, duration, shortDate, type Comment, type HistoryEvent, type Issue, type IssueDetail, type Project, type WikiPage,
} from '../api'
import { Avatar, BotAvatar, Confirm, Empty, Lozenge, Loading, ErrorBox, Modal, PopButton, Tabs, useMe, useProject, useToast } from '../ui'
import { PriorityIcon, TypeIcon, priorityLabel } from '../icons'
import { RichText, RichView, EMPTY_DOC, isEmptyDoc } from './Editor'
import {
  ComponentPicker, CustomFieldValue, DateValue, LabelsEditor, NumberValue, ParentPicker, PersonPicker, PriorityPicker, SprintPicker, StatusPicker, TypePicker, VersionPicker,
} from './fields'

export function useIssue(key: string) {
  return useQuery({ queryKey: ['issue', key.toUpperCase()], queryFn: () => api.get<IssueDetail>(`/issues/${encodeURIComponent(key)}?view=1`) })
}

/** Invalidates everything that shows issues. */
export function refreshIssues(qc: ReturnType<typeof useQueryClient>) {
  for (const k of ['issues', 'board', 'backlog', 'search', 'my-work', 'children', 'timeline', 'calendar', 'report', 'activity']) qc.invalidateQueries({ queryKey: [k] })
}

export default function IssueView({ issueKey, onOpen, inPanel }: { issueKey: string; onOpen?: (key: string) => void; inPanel?: boolean }) {
  const q = useIssue(issueKey)
  const project = useProject(q.data?.projectKey)
  if (q.isLoading || (q.data && project.isLoading)) return <Loading />
  if (q.error) return <div className="page"><ErrorBox error={q.error} /></div>
  if (!q.data || !project.data) return null
  return <IssueBody issue={q.data} project={project.data} onOpen={onOpen} inPanel={inPanel} />
}

function IssueBody({ issue, project, onOpen, inPanel }: { issue: IssueDetail; project: Project; onOpen?: (key: string) => void; inPanel?: boolean }) {
  const qc = useQueryClient()
  const toast = useToast()
  const nav = useNavigate()
  const me = useMe().data
  const can = issue.permissions.edit
  const open = (key: string) => (onOpen ? onOpen(key) : nav(`/issues/${key}`))
  const save = useMutation({
    mutationFn: (patch: Record<string, unknown>) => api.patch<Issue>(`/issues/${issue.key}`, patch),
    onSuccess: (s) => {
      qc.setQueryData(['issue', issue.key], (old: IssueDetail | undefined) => (old ? { ...old, ...s } : old))
      qc.invalidateQueries({ queryKey: ['issue', issue.key] })
      qc.invalidateQueries({ queryKey: ['history', issue.key] })
      refreshIssues(qc)
    },
    onError: (e: Error) => toast(e.message),
  })
  const set = (patch: Record<string, unknown>) => save.mutate(patch)
  const [title, setTitle] = useState(issue.title)
  useEffect(() => setTitle(issue.title), [issue.title])
  const titleRef = useRef<HTMLTextAreaElement>(null)
  useEffect(() => {
    const el = titleRef.current
    if (el) {
      el.style.height = 'auto'
      el.style.height = el.scrollHeight + 'px'
    }
  }, [title])
  const [del, setDel] = useState(false)
  const [linking, setLinking] = useState(false)
  const [tab, setTab] = useState<'comments' | 'history' | 'work'>('comments')
  const fileRef = useRef<HTMLInputElement>(null)
  const [dragging, setDragging] = useState(false)
  const upload = async (files: FileList | File[]) => {
    const form = new FormData()
    Array.from(files).forEach((f) => form.append('file', f))
    try {
      await api.upload(`/issues/${issue.key}/attachments`, form)
      qc.invalidateQueries({ queryKey: ['issue', issue.key] })
      toast(t('Uploaded'))
    } catch (e) {
      toast((e as Error).message)
    }
  }
  const estimation = project.settings.estimation ?? 'points'
  const isSub = issue.type.level < 0
  const childTypes = project.types.filter((x) => (issue.type.level > 0 ? x.level === 0 : x.level < 0))
  const copyLink = () => {
    void navigator.clipboard?.writeText(`${window.location.origin}/issues/${issue.key}`)
    toast(t('Link copied'))
  }

  return (
    <div
      className="issue-layout"
      onDragOver={(e) => {
        if (can && e.dataTransfer.types.includes('Files')) {
          e.preventDefault()
          setDragging(true)
        }
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target) setDragging(false)
      }}
      onDrop={(e) => {
        if (!e.dataTransfer.files.length) return
        e.preventDefault()
        setDragging(false)
        void upload(e.dataTransfer.files)
      }}
    >
      <div className="issue-main">
        <div className="crumbs">
          <Link to={`/projects/${issue.projectKey}`}>{issue.projectName}</Link>
          {issue.parent && (
            <>
              <span>/</span>
              <a href={`/issues/${issue.parent.key}`} onClick={(e) => { e.preventDefault(); open(issue.parent!.key) }}>
                <span className="dot" style={{ background: issue.parent.color ?? 'var(--accent)', marginInlineEnd: 4 }} />
                {issue.parent.key}
              </a>
            </>
          )}
          <span>/</span>
          <span className="row" style={{ gap: 5 }}>
            <TypeIcon icon={issue.type.icon} color={issue.type.color} size={14} />
            <span className="issue-key">{issue.key}</span>
          </span>
          {inPanel && (
            <Link to={`/issues/${issue.key}`} className="icon-btn icon-btn-sm" title={t('Open as page')} aria-label={t('Open as page')}>
              <ExternalLink size={13} />
            </Link>
          )}
        </div>
        <textarea
          ref={titleRef}
          className="issue-title"
          value={title}
          rows={1}
          readOnly={!can}
          aria-label={t('Title')}
          onChange={(e) => setTitle(e.target.value.replace(/\n/g, ' '))}
          onBlur={() => title.trim() && title !== issue.title && set({ title })}
          onKeyDown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault()
              ;(e.target as HTMLTextAreaElement).blur()
            }
            if (e.key === 'Escape') {
              setTitle(issue.title)
              ;(e.target as HTMLTextAreaElement).blur()
            }
          }}
        />
        {can && (
          <div className="row-wrap" style={{ marginBottom: 14 }}>
            <button className="btn btn-sm" onClick={() => fileRef.current?.click()}>
              <Paperclip size={14} /> {t('Attach')}
            </button>
            <input ref={fileRef} type="file" multiple hidden onChange={(e) => e.target.files && upload(e.target.files)} />
            <button className="btn btn-sm" onClick={() => setLinking(true)}>
              <Link2 size={14} /> {t('Link issue')}
            </button>
            {!isSub && childTypes.length > 0 && <AddChild issue={issue} project={project} />}
            <WikiActions issue={issue} />
          </div>
        )}
        {dragging && <div className="dropzone">{t('Drop files to attach them')}</div>}

        <div className="section" style={{ marginTop: 4 }}>
          <div className="section-head"><h3>{t('Description')}</h3></div>
          <RichText
            key={issue.id}
            value={issue.description}
            label={t('Description')}
            editable={can}
            placeholder={t('Describe the work: context, acceptance criteria… Type @ to mention someone, # for an issue.')}
            onSave={(doc) => save.mutateAsync({ description: isEmptyDoc(doc) ? null : doc }).then(() => {})}
          />
        </div>

        {(issue.children.length > 0 || (can && !isSub && childTypes.length > 0)) && issue.children.length > 0 && (
          <Children issue={issue} open={open} />
        )}
        {(issue.links.length > 0 || linking) && <Links issue={issue} adding={linking} onDone={() => setLinking(false)} open={open} can={can} />}
        <WikiPages issue={issue} can={can} />
        {issue.webLinks.some((l) => l.kind === 'web') && (
          <div className="section">
            <div className="section-head"><h3>{t('Web links')}</h3></div>
            {issue.webLinks.filter((l) => l.kind === 'web').map((l) => (
              <div key={l.id} className="wiki-link">
                <ExternalLink size={14} />
                <a href={l.url} target="_blank" rel="noreferrer noopener" style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis' }}>{l.title}</a>
                {can && <RemoveButton onClick={async () => { await api.del(`/web-links/${l.id}`); qc.invalidateQueries({ queryKey: ['issue', issue.key] }) }} />}
              </div>
            ))}
          </div>
        )}
        {issue.attachments.length > 0 && (
          <div className="section">
            <div className="section-head"><h3>{t('Attachments')}</h3><span className="badge">{issue.attachments.length}</span></div>
            <div className="attachments">
              {issue.attachments.map((a) => (
                <div key={a.id} className="attachment">
                  {a.mime.startsWith('image/') && a.mime !== 'image/svg+xml' ? <img src={a.url} alt="" /> : <FileText size={22} className="faint" />}
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <a href={a.url} target="_blank" rel="noreferrer" style={{ display: 'block', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{a.name}</a>
                    <div className="faint small">{(a.size / 1024).toFixed(a.size > 1024 * 100 ? 0 : 1)} KB · {ago(a.createdAt)}</div>
                  </div>
                  {can && <RemoveButton onClick={async () => { await api.del(`/attachments/${a.id}`); qc.invalidateQueries({ queryKey: ['issue', issue.key] }) }} />}
                </div>
              ))}
            </div>
          </div>
        )}

        <div className="section">
          <Tabs
            value={tab}
            onChange={setTab}
            tabs={[
              { id: 'comments', label: <span className="row" style={{ gap: 6 }}><MessageSquare size={14} />{t('Comments')}{issue.commentCount > 0 && <span className="badge">{issue.commentCount}</span>}</span> },
              { id: 'history', label: <span className="row" style={{ gap: 6 }}><Activity size={14} />{t('History')}</span> },
              { id: 'work', label: <span className="row" style={{ gap: 6 }}><Timer size={14} />{t('Time')}</span> },
            ]}
          />
          {tab === 'comments' && <Comments issue={issue} meId={me?.user?.id} />}
          {tab === 'history' && <History issueKey={issue.key} />}
          {tab === 'work' && <WorkLog issue={issue} />}
        </div>
      </div>

      <aside className="issue-side">
        <div className="row" style={{ marginBottom: 12 }}>
          <StatusPicker project={project} status={issue.status} disabled={!can} onChange={(id) => set({ statusId: id })} />
          {issue.blocked && (
            <span className="badge badge-danger" title={t('An open issue blocks this one')}>
              <Ban size={12} /> {t('Blocked')}
            </span>
          )}
          <span className="spacer" />
          <button className="icon-btn" title={issue.watching ? t('Stop watching') : t('Watch')} aria-label={issue.watching ? t('Stop watching') : t('Watch')} onClick={async () => { await api.put(`/issues/${issue.key}/watch`, { watching: !issue.watching }); qc.invalidateQueries({ queryKey: ['issue', issue.key] }) }}>
            {issue.watching ? <Eye size={16} /> : <EyeOff size={16} />}
          </button>
          <span className="faint small">{issue.watchers.length}</span>
          <PopButton className="icon-btn" title={t('More')} label={<Ellipsis size={16} />} align="end" width={210}>
            {(close) => (
              <div>
                <button className="menu-item" onClick={() => { close(); copyLink() }}><Copy size={14} /> {t('Copy link')}</button>
                {can && (
                  <button
                    className="menu-item"
                    onClick={async () => {
                      close()
                      const c = await api.post<Issue>(`/issues/${issue.key}/clone`)
                      refreshIssues(qc)
                      toast(t('Created {key}', { key: c.key }))
                      open(c.key)
                    }}
                  >
                    <Copy size={14} /> {t('Clone')}
                  </button>
                )}
                {can && <button className="menu-item danger" onClick={() => { close(); setDel(true) }}><Trash size={14} /> {t('Delete')}</button>}
              </div>
            )}
          </PopButton>
        </div>

        <SideField label={t('Assignee')}>
          <PersonPicker value={issue.assignee} disabled={!can} onChange={(id) => set({ assigneeId: id })} />
        </SideField>
        {can && !issue.assignee && me?.user && (
          <div style={{ margin: '-4px 0 4px 108px' }}>
            <button className="btn btn-ghost btn-sm" style={{ paddingInline: 4, color: 'var(--accent-text)' }} onClick={() => set({ assigneeId: me.user!.id })}>{t('Assign to me')}</button>
          </div>
        )}
        <SideField label={t('Reporter')}>
          <PersonPicker value={issue.reporter} disabled={!can} allowNone={false} label={t('Reporter')} onChange={(id) => set({ reporterId: id })} />
        </SideField>
        <SideField label={t('Priority')}>
          <PriorityPicker value={issue.priority} disabled={!can} onChange={(p) => set({ priority: p })} />
        </SideField>
        <SideField label={t('Type')}>
          <TypePicker project={project} value={issue.type} disabled={!can} onChange={(id) => set({ typeId: id })} />
        </SideField>
        {issue.type.level <= 0 && (
          <SideField label={isSub ? t('Parent') : t('Epic')}>
            <ParentPicker value={issue.parent} disabled={!can} epicsOnly={!isSub} projectKey={issue.projectKey} onChange={(id) => set({ parentId: id })} />
          </SideField>
        )}
        {project.kind === 'scrum' && issue.type.level <= 0 && (
          <SideField label={t('Sprint')}>
            <SprintPicker project={project} value={issue.sprint} disabled={!can} onChange={(id) => set({ sprintId: id })} />
          </SideField>
        )}
        {project.versions.length > 0 && (
          <SideField label={t('Release')}>
            <VersionPicker project={project} value={issue.version} disabled={!can} onChange={(id) => set({ versionId: id })} />
          </SideField>
        )}
        {project.components.length > 0 && (
          <SideField label={t('Component')}>
            <ComponentPicker project={project} value={issue.component} disabled={!can} onChange={(id) => set({ componentId: id })} />
          </SideField>
        )}
        <SideField label={t('Labels')}>
          <LabelsEditor value={issue.labels} disabled={!can} onChange={(l) => set({ labels: l })} />
        </SideField>
        {estimation === 'points' ? (
          <SideField label={t('Story points')}>
            <NumberValue value={issue.storyPoints} disabled={!can} onChange={(n) => set({ storyPoints: n })} />
          </SideField>
        ) : null}
        <SideField label={t('Estimate')}>
          <NumberValue value={issue.estimateMinutes != null ? Math.round((issue.estimateMinutes / 60) * 10) / 10 : null} suffix={t('hours')} disabled={!can} onChange={(n) => set({ estimateMinutes: n == null ? null : Math.round(n * 60) })} />
        </SideField>
        <SideField label={t('Time spent')}>
          <TimeBar spent={issue.spentMinutes} estimate={issue.estimateMinutes} />
        </SideField>
        <SideField label={t('Start')}>
          <DateValue value={issue.startDate} disabled={!can} onChange={(d) => set({ startDate: d })} />
        </SideField>
        <SideField label={t('Due')}>
          <DateValue value={issue.dueDate} disabled={!can} highlightOverdue={issue.status.category !== 'done'} onChange={(d) => set({ dueDate: d })} />
        </SideField>
        {project.fields.length > 0 && <div className="divider" style={{ margin: '10px 0' }} />}
        {project.fields.map((f) => (
          <SideField key={f.id} label={f.name}>
            <CustomFieldValue field={f} value={issue.custom?.[f.id]} disabled={!can} onChange={(v) => set({ custom: { [f.id]: v } })} />
          </SideField>
        ))}
        <div className="divider" style={{ margin: '12px 0' }} />
        <div className="faint small col" style={{ gap: 3 }}>
          <span>{t('Created {when}', { when: dateTime(issue.createdAt) })}</span>
          <span>{t('Updated {when}', { when: ago(issue.updatedAt) })}</span>
          {issue.resolvedAt && <span>{t('Resolved {when}', { when: dateTime(issue.resolvedAt) })}</span>}
          <span>{t('In this status since {when}', { when: ago(issue.statusChangedAt) })}</span>
          {issue.watchers.length > 0 && <span>{t('Watching: {names}', { names: issue.watchers.map((w) => w.name).join(', ') })}</span>}
        </div>
      </aside>
      {del && (
        <Confirm
          title={t('Delete {key}?', { key: issue.key })}
          body={<p>{t('The issue moves to the project’s trash, where it can be restored.')}</p>}
          confirmLabel={t('Delete')}
          danger
          onClose={() => setDel(false)}
          onConfirm={async () => {
            await api.del(`/issues/${issue.key}`)
            refreshIssues(qc)
            toast(t('{key} deleted', { key: issue.key }), {
              label: t('Undo'),
              run: async () => {
                await api.post(`/issues/${issue.key}/restore`)
                refreshIssues(qc)
              },
            })
            if (inPanel) {
              const url = new URL(window.location.href)
              url.searchParams.delete('issue')
              nav(url.pathname + url.search)
            } else nav(`/projects/${issue.projectKey}`)
          }}
        />
      )}
    </div>
  )
}

function SideField({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="side-field">
      <span className="sf-label">{label}</span>
      <div className="sf-value">{children}</div>
    </div>
  )
}

function RemoveButton({ onClick }: { onClick: () => void }) {
  return (
    <button className="icon-btn icon-btn-sm" aria-label={t('Remove')} title={t('Remove')} onClick={onClick}>
      <X size={14} />
    </button>
  )
}

function TimeBar({ spent, estimate }: { spent: number; estimate: number | null }) {
  const pct = estimate ? Math.min(100, (spent / estimate) * 100) : spent ? 100 : 0
  const over = estimate != null && spent > estimate
  return (
    <div style={{ padding: '4px 0' }}>
      <div className="progress" style={{ width: 160 }}>
        <span style={{ width: `${pct}%`, background: over ? 'var(--danger)' : 'var(--accent)' }} />
      </div>
      <div className="faint small" style={{ marginTop: 3 }}>
        {duration(spent)} {t('logged')}
        {estimate != null && ` · ${over ? t('{time} over', { time: duration(spent - estimate) }) : t('{time} left', { time: duration(estimate - spent) })}`}
      </div>
    </div>
  )
}

function Children({ issue, open }: { issue: IssueDetail; open: (k: string) => void }) {
  const done = issue.children.filter((c) => c.status.category === 'done').length
  const pct = issue.children.length ? Math.round((done / issue.children.length) * 100) : 0
  return (
    <div className="section">
      <div className="section-head">
        <h3>{issue.type.level > 0 ? t('Issues in this epic') : t('Sub-tasks')}</h3>
        <span className="faint small">{t('{done} of {total} done', { done, total: issue.children.length })}</span>
        <div className="progress" style={{ width: 120 }}>
          <span className="p-done" style={{ width: `${pct}%` }} />
        </div>
      </div>
      <div>
        {issue.children.map((c) => (
          <div key={c.id} className="child-row" onClick={() => open(c.key)}>
            <TypeIcon icon={c.type.icon} color={c.type.color} />
            <span className="issue-key">{c.key}</span>
            <span style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', textDecoration: c.status.category === 'done' ? 'line-through' : undefined }}>{c.title}</span>
            <PriorityIcon priority={c.priority} />
            {c.assignee ? <Avatar name={c.assignee.name} id={c.assignee.id} size={20} /> : null}
            <Lozenge name={c.status.name} category={c.status.category} />
          </div>
        ))}
      </div>
    </div>
  )
}

function AddChild({ issue, project }: { issue: IssueDetail; project: Project }) {
  const qc = useQueryClient()
  const toast = useToast()
  const types = project.types.filter((x) => (issue.type.level > 0 ? x.level === 0 : x.level < 0))
  return (
    <PopButton className="btn btn-sm" label={<><CornerDownRight size={14} /> {issue.type.level > 0 ? t('Add issue') : t('Add sub-task')}</>} width={360}>
      {(close) => (
        <form
          style={{ padding: 6 }}
          onSubmit={async (e) => {
            e.preventDefault()
            const fd = new FormData(e.currentTarget)
            const title = String(fd.get('title') ?? '').trim()
            if (!title) return
            try {
              const c = await api.post<Issue>('/issues', {
                projectKey: issue.projectKey,
                title,
                typeId: fd.get('type'),
                parentId: issue.id,
                ...(issue.sprint && issue.type.level === 0 ? { sprintId: issue.sprint.id } : {}),
              })
              qc.invalidateQueries({ queryKey: ['issue', issue.key] })
              refreshIssues(qc)
              toast(t('Created {key}', { key: c.key }))
              close()
            } catch (err) {
              toast((err as Error).message)
            }
          }}
        >
          <input name="title" className="input" autoFocus placeholder={t('What needs to be done?')} />
          <div className="row" style={{ marginTop: 8 }}>
            <select name="type" className="select input-sm" style={{ width: 'auto' }} defaultValue={types[0]?.id}>
              {types.map((x) => <option key={x.id} value={x.id}>{x.name}</option>)}
            </select>
            <span className="spacer" />
            <button className="btn btn-primary btn-sm" type="submit">{t('Create')}</button>
          </div>
        </form>
      )}
    </PopButton>
  )
}

/** Link and field names as people read them. */
const LINK_LABELS: Record<string, () => string> = {
  blocks: () => t('blocks'),
  'is blocked by': () => t('is blocked by'),
  relates: () => t('relates to'),
  'relates to': () => t('relates to'),
  duplicates: () => t('duplicates'),
  'is duplicated by': () => t('is duplicated by'),
  clones: () => t('clones'),
  'is cloned by': () => t('is cloned by'),
}
const linkLabel = (l: string) => LINK_LABELS[l]?.() ?? l

const FIELD_LABELS: Record<string, () => string> = {
  title: () => t('the title'),
  type: () => t('the type'),
  assignee: () => t('the assignee'),
  reporter: () => t('the reporter'),
  parent: () => t('the parent'),
  sprint: () => t('the sprint'),
  version: () => t('the release'),
  component: () => t('the component'),
  points: () => t('the story points'),
}

const LINK_KINDS = [
  { id: 'blocks', label: () => t('blocks') },
  { id: 'is blocked by', label: () => t('is blocked by') },
  { id: 'relates', label: () => t('relates to') },
  { id: 'duplicates', label: () => t('duplicates') },
  { id: 'is duplicated by', label: () => t('is duplicated by') },
]

function Links({ issue, adding, onDone, open, can }: { issue: IssueDetail; adding: boolean; onDone: () => void; open: (k: string) => void; can: boolean }) {
  const qc = useQueryClient()
  const toast = useToast()
  const [kind, setKind] = useState('relates')
  const [q, setQ] = useState('')
  const found = useQuery({ queryKey: ['link-search', q], queryFn: () => api.get<Issue[]>(`/quick?q=${encodeURIComponent(q)}`), enabled: adding })
  const groups = new Map<string, typeof issue.links>()
  for (const l of issue.links) groups.set(l.label, [...(groups.get(l.label) ?? []), l])
  const add = async (target: Issue) => {
    try {
      await api.post(`/issues/${issue.key}/links`, { kind, target: target.key })
      qc.invalidateQueries({ queryKey: ['issue', issue.key] })
      refreshIssues(qc)
      setQ('')
      onDone()
    } catch (e) {
      toast((e as Error).message)
    }
  }
  return (
    <div className="section">
      <div className="section-head"><h3>{t('Linked issues')}</h3></div>
      {adding && (
        <div className="card" style={{ padding: 10, marginBottom: 8 }}>
          <div className="row">
            <select className="select input-sm" style={{ width: 'auto' }} value={kind} onChange={(e) => setKind(e.target.value)} aria-label={t('Link type')}>
              {LINK_KINDS.map((k) => <option key={k.id} value={k.id}>{k.label()}</option>)}
            </select>
            <input className="input input-sm" autoFocus placeholder={t('Search issues by key or words…')} value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={(e) => e.key === 'Escape' && onDone()} />
            <button className="icon-btn" onClick={onDone} aria-label={t('Cancel')}><X size={15} /></button>
          </div>
          <div className="choices-list" style={{ marginTop: 6, maxHeight: 220, overflowY: 'auto' }}>
            {(found.data ?? []).filter((i) => i.id !== issue.id).map((i) => (
              <button key={i.id} className="menu-item" onClick={() => add(i)}>
                <TypeIcon icon={i.type.icon} color={i.type.color} />
                <span className="issue-key">{i.key}</span>
                <span style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{i.title}</span>
                <Lozenge name={i.status.name} category={i.status.category} />
              </button>
            ))}
          </div>
        </div>
      )}
      {[...groups.entries()].map(([label, links]) => (
        <div key={label} style={{ marginBottom: 8 }}>
          <div className="faint small" style={{ marginBottom: 4 }}>{linkLabel(label)}</div>
          {links.map((l) => (
            <div key={l.id} className="child-row" onClick={() => open(l.issue.key)}>
              <TypeIcon icon={l.issue.type.icon} color={l.issue.type.color} />
              <span className="issue-key">{l.issue.key}</span>
              <span style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{l.issue.title}</span>
              <Lozenge name={l.issue.status.name} category={l.issue.status.category} />
              {can && (
                <RemoveButton
                  onClick={async () => {
                    await api.del(`/links/${l.id}`)
                    qc.invalidateQueries({ queryKey: ['issue', issue.key] })
                    refreshIssues(qc)
                  }}
                />
              )}
            </div>
          ))}
        </div>
      ))}
    </div>
  )
}

// ---- WikiCraft ----

function WikiActions({ issue }: { issue: IssueDetail }) {
  const me = useMe().data
  const qc = useQueryClient()
  const toast = useToast()
  const [mode, setMode] = useState<'link' | 'create' | null>(null)
  if (!me?.wikicraft?.connected) {
    return (
      <PopButton className="btn btn-sm" label={<><Plus size={14} /> {t('Web link')}</>} width={360}>
        {(close) => <WebLinkForm issue={issue} onDone={close} />}
      </PopButton>
    )
  }
  return (
    <>
      <PopButton className="btn btn-sm" label={<><BookOpen size={14} /> {t('Wiki page')}</>} width={260}>
        {(close) => (
          <div>
            <button className="menu-item" onClick={() => { close(); setMode('link') }}><Link2 size={14} /> {t('Link a wiki page…')}</button>
            <button className="menu-item" onClick={() => { close(); setMode('create') }}><Plus size={14} /> {t('Write a page for this issue…')}</button>
            <div className="divider" />
            <PopButton className="menu-item" label={<><ExternalLink size={14} /> {t('Other web link…')}</>} width={360}>
              {(c2) => <WebLinkForm issue={issue} onDone={() => { c2(); close() }} />}
            </PopButton>
          </div>
        )}
      </PopButton>
      {mode === 'link' && (
        <WikiSearchDialog
          title={t('Link a wiki page to {key}', { key: issue.key })}
          onClose={() => setMode(null)}
          onPick={async (p) => {
            try {
              await api.post(`/issues/${issue.key}/wiki-pages`, { url: p.url })
              qc.invalidateQueries({ queryKey: ['wiki-pages', issue.key] })
              qc.invalidateQueries({ queryKey: ['issue', issue.key] })
              setMode(null)
            } catch (e) {
              toast((e as Error).message)
            }
          }}
        />
      )}
      {mode === 'create' && <CreateWikiPage issue={issue} onClose={() => setMode(null)} />}
    </>
  )
}

function WebLinkForm({ issue, onDone }: { issue: IssueDetail; onDone: () => void }) {
  const qc = useQueryClient()
  const toast = useToast()
  return (
    <form
      style={{ padding: 6 }}
      onSubmit={async (e) => {
        e.preventDefault()
        const fd = new FormData(e.currentTarget)
        try {
          await api.post(`/issues/${issue.key}/web-links`, { url: fd.get('url'), title: fd.get('title') })
          qc.invalidateQueries({ queryKey: ['issue', issue.key] })
          qc.invalidateQueries({ queryKey: ['wiki-pages', issue.key] })
          onDone()
        } catch (err) {
          toast((err as Error).message)
        }
      }}
    >
      <div className="field"><label>{t('Address')}</label><input name="url" className="input" autoFocus placeholder="https://" /></div>
      <div className="field"><label>{t('Text (optional)')}</label><input name="title" className="input" /></div>
      <button className="btn btn-primary btn-sm" type="submit">{t('Add link')}</button>
    </form>
  )
}

export function WikiSearchDialog({ title, onClose, onPick, space }: { title: string; onClose: () => void; onPick: (p: WikiPage) => void; space?: string }) {
  const [q, setQ] = useState('')
  const found = useQuery({
    queryKey: ['wiki-search', q, space],
    queryFn: () => api.get<WikiPage[]>(`/integrations/wikicraft/search?q=${encodeURIComponent(q)}${space ? `&space=${encodeURIComponent(space)}` : ''}`),
    enabled: q.trim().length > 1 || !!space,
  })
  return (
    <Modal title={title} onClose={onClose} width={560}>
      <input className="input" autoFocus placeholder={t('Search the wiki or paste a page address…')} value={q} onChange={(e) => setQ(e.target.value)} />
      <div className="choices-list" style={{ marginTop: 8, minHeight: 120 }}>
        {found.isFetching && <div className="menu-empty">{t('Searching…')}</div>}
        {found.error && <div className="error-text">{(found.error as Error).message}</div>}
        {(found.data ?? []).map((p) => (
          <button key={p.id} className="menu-item" onClick={() => onPick(p)} style={{ alignItems: 'flex-start' }}>
            <BookOpen size={15} style={{ marginTop: 3 }} />
            <span style={{ flex: 1, minWidth: 0 }}>
              <div>{p.title}</div>
              <div className="mi-desc">{p.spaceName || p.spaceKey}{p.excerpt ? ` · ${p.excerpt.slice(0, 90)}` : ''}</div>
            </span>
          </button>
        ))}
        {found.data && found.data.length === 0 && <div className="menu-empty">{t('No pages found')}</div>}
      </div>
    </Modal>
  )
}

function CreateWikiPage({ issue, onClose }: { issue: IssueDetail; onClose: () => void }) {
  const project = useProject(issue.projectKey).data
  const spaces = useQuery({ queryKey: ['wiki-spaces'], queryFn: () => api.get<{ key: string; name: string }[]>('/integrations/wikicraft/spaces') })
  const [space, setSpace] = useState(project?.settings.wikiSpace ?? '')
  const [title, setTitle] = useState(`${issue.key}: ${issue.title}`)
  const [busy, setBusy] = useState(false)
  const qc = useQueryClient()
  const toast = useToast()
  return (
    <Modal
      title={t('Write a wiki page for {key}', { key: issue.key })}
      onClose={onClose}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button
            className="btn btn-primary"
            disabled={busy || !space}
            onClick={async () => {
              setBusy(true)
              try {
                const p = await api.post<WikiPage>(`/issues/${issue.key}/wiki-page`, { spaceKey: space, title })
                qc.invalidateQueries({ queryKey: ['wiki-pages', issue.key] })
                qc.invalidateQueries({ queryKey: ['issue', issue.key] })
                toast(t('Page created'), { label: t('Open [[action]]'), run: () => window.open(p.url, '_blank', 'noopener') })
                onClose()
              } catch (e) {
                toast((e as Error).message)
              } finally {
                setBusy(false)
              }
            }}
          >{t('Create page')}</button>
        </>
      }
    >
      <p className="muted" style={{ marginTop: 0 }}>{t('The page starts from the issue’s description with sections for goals, approach and open questions, and is linked to the issue.')}</p>
      <div className="field">
        <label>{t('Space')}</label>
        <select className="select" value={space} onChange={(e) => setSpace(e.target.value)}>
          <option value="">{t('Choose a space…')}</option>
          {(spaces.data ?? []).map((s) => <option key={s.key} value={s.key}>{s.name} ({s.key})</option>)}
        </select>
        {spaces.error && <span className="error-text">{(spaces.error as Error).message}</span>}
      </div>
      <div className="field">
        <label>{t('Title')}</label>
        <input className="input" value={title} onChange={(e) => setTitle(e.target.value)} />
      </div>
    </Modal>
  )
}

function WikiPages({ issue, can }: { issue: IssueDetail; can: boolean }) {
  const qc = useQueryClient()
  const hasWiki = issue.webLinks.some((l) => l.kind === 'wiki')
  const pages = useQuery({
    queryKey: ['wiki-pages', issue.key],
    queryFn: () => api.get<{ id: string; url: string; title: string; page: WikiPage | null; available: boolean }[]>(`/issues/${issue.key}/wiki-pages`),
    enabled: hasWiki,
  })
  if (!hasWiki) return null
  return (
    <div className="section">
      <div className="section-head"><h3>{t('Wiki pages')}</h3></div>
      {(pages.data ?? issue.webLinks.filter((l) => l.kind === 'wiki').map((l) => ({ id: l.id, url: l.url, title: l.title, page: null, available: true }))).map((p) => (
        <div key={p.id} className="wiki-link">
          <BookOpen size={15} className="faint" />
          <div style={{ flex: 1, minWidth: 0 }}>
            <a href={p.url} target="_blank" rel="noreferrer noopener">{p.title}</a>
            <div className="faint small">
              {p.page ? `${p.page.spaceName || p.page.spaceKey}${p.page.updatedAt ? ` · ${t('updated {when}', { when: ago(p.page.updatedAt) })}` : ''}` : pages.data && !p.available ? t('Not available to you (or deleted)') : ''}
            </div>
          </div>
          {can && (
            <RemoveButton
              onClick={async () => {
                await api.del(`/web-links/${p.id}`)
                qc.invalidateQueries({ queryKey: ['wiki-pages', issue.key] })
                qc.invalidateQueries({ queryKey: ['issue', issue.key] })
              }}
            />
          )}
        </div>
      ))}
    </div>
  )
}

// ---- comments, history, time ----

const REACTIONS: { id: string; label: string }[] = [
  { id: 'thumbs-up', label: '👍' },
  { id: 'heart', label: '❤️' },
  { id: 'check', label: '✅' },
  { id: 'eyes', label: '👀' },
  { id: 'party', label: '🎉' },
]

function Comments({ issue, meId }: { issue: IssueDetail; meId?: string }) {
  const qc = useQueryClient()
  const toast = useToast()
  const list = useQuery({ queryKey: ['comments', issue.key], queryFn: () => api.get<Comment[]>(`/issues/${issue.key}/comments`) })
  const [editing, setEditing] = useState<string | null>(null)
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['comments', issue.key] })
    qc.invalidateQueries({ queryKey: ['issue', issue.key] })
  }
  return (
    <div className="col" style={{ gap: 12 }}>
      {issue.permissions.comment && (
        <div className="row" style={{ alignItems: 'flex-start', gap: 10 }}>
          <Avatar name={useMe().data?.user?.name ?? ''} id={meId} size={28} />
          <div style={{ flex: 1, minWidth: 0 }}>
            <RichText
              key={`new-${issue.id}`}
              boxed
              value={EMPTY_DOC}
              placeholder={t('Add a comment… @ to mention')}
              label={t('New comment')}
              saveLabel={t('Comment')}
              onSave={async (doc) => {
                if (isEmptyDoc(doc)) return
                try {
                  await api.post(`/issues/${issue.key}/comments`, { body: doc })
                  refresh()
                } catch (e) {
                  toast((e as Error).message)
                  throw e
                }
              }}
            />
          </div>
        </div>
      )}
      {list.isLoading && <Loading />}
      {[...(list.data ?? [])].reverse().map((c) => (
        <div key={c.id} className="row" style={{ alignItems: 'flex-start', gap: 10 }}>
          {c.author ? <Avatar name={c.author.name} id={c.author.id} size={28} /> : <BotAvatar size={28} />}
          <div style={{ flex: 1, minWidth: 0 }}>
            <div className="comment-head">
              <strong>{c.author?.name ?? t('Automation')}</strong>
              <span className="faint" title={dateTime(c.createdAt)}>{ago(c.createdAt)}</span>
              {c.edited && <span className="faint small">({t('edited')})</span>}
              <span className="spacer" />
              {c.author?.id === meId && editing !== c.id && (
                <>
                  <button className="btn btn-ghost btn-sm" onClick={() => setEditing(c.id)}>{t('Edit')}</button>
                  <button className="btn btn-ghost btn-sm" onClick={async () => { await api.del(`/comments/${c.id}`); refresh() }}>{t('Delete')}</button>
                </>
              )}
            </div>
            {editing === c.id ? (
              <RichText
                boxed
                autoFocus
                value={c.body}
                onSave={async (doc) => {
                  await api.patch(`/comments/${c.id}`, { body: doc })
                  setEditing(null)
                  refresh()
                }}
              />
            ) : (
              <div className="comment-box"><RichView doc={c.body} label={t('Comment by {name}', { name: c.author?.name ?? t('Automation') })} /></div>
            )}
            <div className="reactions">
              {c.reactions.map((r) => (
                <button key={r.reaction} className={`reaction ${r.people.some((p) => p.id === meId) ? 'mine' : ''}`} title={r.people.map((p) => p.name).join(', ')} onClick={async () => { await api.post(`/comments/${c.id}/react`, { reaction: r.reaction }); refresh() }}>
                  {REACTIONS.find((x) => x.id === r.reaction)?.label} {r.people.length}
                </button>
              ))}
              <PopButton className="reaction" title={t('Add a reaction')} label={<><Heart size={12} aria-hidden /><Plus size={11} aria-hidden /></>} width={200}>
                {(close) => (
                  <div className="row" style={{ padding: 4 }}>
                    {REACTIONS.map((r) => (
                      <button key={r.id} className="icon-btn" aria-label={r.id} onClick={async () => { close(); await api.post(`/comments/${c.id}/react`, { reaction: r.id }); refresh() }}>
                        {r.label}
                      </button>
                    ))}
                  </div>
                )}
              </PopButton>
            </div>
          </div>
        </div>
      ))}
      {list.data?.length === 0 && !issue.permissions.comment && <Empty title={t('No comments yet')} />}
    </div>
  )
}

/** One history entry as a sentence. */
export function describeEvent(e: HistoryEvent): React.ReactNode {
  const name = (v: any) => (v == null ? t('none') : typeof v === 'object' ? (v.name ?? v.key ?? v.value ?? JSON.stringify(v)) : String(v))
  switch (e.kind) {
    case 'created':
      return t('created the issue')
    case 'deleted':
      return t('deleted the issue')
    case 'restored':
      return t('restored the issue')
    case 'comment':
      return <>{t('commented')}{e.new?.excerpt && <span className="faint">: “{String(e.new.excerpt).slice(0, 120)}”</span>}</>
    case 'worklog':
      return t('logged {time}', { time: duration(e.new?.minutes) })
    case 'attachment':
      return e.new ? t('attached {name}', { name: e.new.name }) : t('removed the attachment {name}', { name: e.old?.name })
    case 'link':
      return e.field === 'webLink' ? t('linked {title}', { title: e.new?.title ?? e.new?.url }) : <>{t('linked')} <span className="faint">{linkLabel(String(e.new?.kind ?? ''))}</span> <span className="issue-key">{e.new?.key}</span></>
  }
  const field = e.field ?? ''
  switch (field) {
    case 'status':
      return (
        <>
          {t('changed the status')} <Lozenge name={name(e.old)} category={e.old?.category ?? 'todo'} /> → <Lozenge name={name(e.new)} category={e.new?.category ?? 'todo'} />
        </>
      )
    case 'description':
      return t('updated the description')
    case 'priority':
      return t('changed the priority from {a} to {b}', { a: priorityLabel(e.old), b: priorityLabel(e.new) })
    case 'labels':
      return t('changed the labels to {labels}', { labels: (e.new ?? []).join(', ') || t('none') })
    case 'custom':
      return t('changed {field} to {value}', { field: e.new?.field ?? '', value: name(e.new?.value) })
    case 'due':
    case 'start':
      return t('changed the {field} date to {date}', { field: field === 'due' ? t('due') : t('start'), date: e.new ? shortDate(e.new) : t('none') })
    case 'estimate':
      return t('changed the estimate to {time}', { time: e.new == null ? t('none') : duration(e.new) })
    default:
      return t('changed {field} from {a} to {b}', { field: FIELD_LABELS[field]?.() ?? field, a: name(e.old), b: name(e.new) })
  }
}

function History({ issueKey }: { issueKey: string }) {
  const h = useQuery({ queryKey: ['history', issueKey], queryFn: () => api.get<HistoryEvent[]>(`/issues/${issueKey}/history`) })
  if (h.isLoading) return <Loading />
  return (
    <div>
      {[...(h.data ?? [])].reverse().map((e) => (
        <div key={e.id} className="activity-item">
          {e.actor ? <Avatar name={e.actor.name} id={e.actor.id} size={24} /> : <BotAvatar size={24} />}
          <div className="ai-body">
            <strong>{e.actor?.name ?? t('Automation')}</strong> {describeEvent(e)}
            <div className="faint small" title={dateTime(e.at)}>{ago(e.at)}</div>
          </div>
        </div>
      ))}
    </div>
  )
}

function WorkLog({ issue }: { issue: IssueDetail }) {
  const qc = useQueryClient()
  const toast = useToast()
  const me = useMe().data?.user
  const list = useQuery({ queryKey: ['worklogs', issue.key], queryFn: () => api.get<{ id: string; user: { id: string; name: string } | null; minutes: number; date: string; note: string | null }[]>(`/issues/${issue.key}/worklogs`) })
  const [time, setTime] = useState('')
  const [date, setDate] = useState(new Date().toISOString().slice(0, 10))
  const [note, setNote] = useState('')
  const [reduce, setReduce] = useState(true)
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['worklogs', issue.key] })
    qc.invalidateQueries({ queryKey: ['issue', issue.key] })
    qc.invalidateQueries({ queryKey: ['timesheet'] })
  }
  return (
    <div className="col" style={{ gap: 10 }}>
      {issue.permissions.edit && (
        <form
          className="row-wrap"
          onSubmit={async (e) => {
            e.preventDefault()
            try {
              await api.post(`/issues/${issue.key}/worklogs`, { time, date, note, reduceEstimate: reduce && issue.estimateMinutes != null })
              setTime('')
              setNote('')
              refresh()
            } catch (err) {
              toast((err as Error).message)
            }
          }}
        >
          <Clock size={15} className="faint" />
          <input className="input input-sm" style={{ width: 110 }} placeholder={t('1h 30m')} value={time} onChange={(e) => setTime(e.target.value)} aria-label={t('Time spent')} />
          <input type="date" className="input input-sm" style={{ width: 150 }} value={date} onChange={(e) => setDate(e.target.value)} aria-label={t('Date')} />
          <input className="input input-sm" style={{ flex: 1, minWidth: 140 }} placeholder={t('What did you do? (optional)')} value={note} onChange={(e) => setNote(e.target.value)} />
          {issue.estimateMinutes != null && (
            <label className="row small muted" style={{ gap: 4 }}>
              <input type="checkbox" checked={reduce} onChange={(e) => setReduce(e.target.checked)} /> {t('reduce the estimate')}
            </label>
          )}
          <button className="btn btn-primary btn-sm" disabled={!time.trim()}>{t('Log time')}</button>
        </form>
      )}
      <table className="list">
        <tbody>
          {(list.data ?? []).map((w) => (
            <tr key={w.id}>
              <td style={{ width: 110 }}>{shortDate(w.date)}</td>
              <td style={{ width: 90 }}><strong>{duration(w.minutes)}</strong></td>
              <td>{w.user?.name}</td>
              <td className="muted">{w.note}</td>
              <td style={{ width: 40 }}>
                {(w.user?.id === me?.id || issue.permissions.admin) && (
                  <button className="icon-btn icon-btn-sm" aria-label={t('Delete')} onClick={async () => { await api.del(`/worklogs/${w.id}`); refresh() }}>
                    <Trash size={13} />
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {list.data?.length === 0 && <div className="faint small">{t('No time logged yet.')}</div>}
      {list.data && list.data.length > 0 && <div className="faint small"><Check size={12} /> {t('{time} in total', { time: duration(issue.spentMinutes) })}</div>}
    </div>
  )
}
