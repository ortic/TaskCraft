import { t } from '../i18n'
import { useEffect, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type Issue, type Priority } from '../api'
import { Modal, useProject, useProjects, useToast } from '../ui'
import { TypeIcon } from '../icons'
import { RichText, EMPTY_DOC, isEmptyDoc } from './Editor'
import { DateValue, LabelsEditor, ParentPicker, PersonPicker, PriorityPicker } from './fields'
import { refreshIssues } from './IssueView'
import type { Template } from './TemplateSettings'

/** `{date}`, `{week}`, `{month}` in a template title, as the server fills them. */
function fillTitle(title: string): string {
  const d = new Date()
  const iso = (x: Date) => x.toISOString().slice(0, 10)
  const thursday = new Date(Date.UTC(d.getFullYear(), d.getMonth(), d.getDate()))
  thursday.setUTCDate(thursday.getUTCDate() + 4 - (thursday.getUTCDay() || 7))
  const week = Math.ceil(((thursday.getTime() - Date.UTC(thursday.getUTCFullYear(), 0, 1)) / 86400000 + 1) / 7)
  return title.replace('{date}', iso(d)).replace('{week}', `${thursday.getUTCFullYear()}-W${String(week).padStart(2, '0')}`).replace('{month}', iso(d).slice(0, 7))
}

export interface CreateDefaults {
  projectKey?: string
  statusId?: string
  sprintId?: string | null
  parentId?: string
  typeId?: string
  dueDate?: string
  startDate?: string
  assigneeId?: string
}

const LAST_PROJECT = 'tc-last-project'

export default function CreateIssue({ defaults, onClose, onCreated }: { defaults?: CreateDefaults; onClose: () => void; onCreated?: (i: Issue) => void }) {
  const projects = useProjects()
  const qc = useQueryClient()
  const toast = useToast()
  const writable = (projects.data ?? []).filter((p) => !p.archivedAt && (p.role === 'member' || p.role === 'admin'))
  const remembered = (() => {
    try {
      return localStorage.getItem(LAST_PROJECT) ?? undefined
    } catch {
      return undefined
    }
  })()
  const [projectKey, setProjectKey] = useState<string | undefined>(defaults?.projectKey ?? remembered)
  useEffect(() => {
    if (!projectKey && writable.length) setProjectKey(writable[0].key)
    if (projectKey && writable.length && !writable.some((p) => p.key === projectKey)) setProjectKey(writable[0].key)
  }, [projectKey, writable])
  const project = useProject(projectKey).data
  const [typeId, setTypeId] = useState<string | undefined>(defaults?.typeId)
  const templates = useQuery({ queryKey: ['templates', projectKey], queryFn: () => api.get<Template[]>(`/projects/${projectKey}/templates`), enabled: !!projectKey })
  const [templateId, setTemplateId] = useState('')
  const type = project?.types.find((x) => x.id === typeId) ?? project?.types.find((x) => x.level === 0) ?? project?.types[0]
  const [title, setTitle] = useState('')
  const [desc, setDesc] = useState<any>(null)
  const [descKey, setDescKey] = useState(0)
  const [assignee, setAssignee] = useState<{ id: string; name: string } | null>(null)
  const [priority, setPriority] = useState<Priority>('medium')
  const [labels, setLabels] = useState<string[]>([])
  const [due, setDue] = useState<string | null>(defaults?.dueDate ?? null)
  const [parent, setParent] = useState<Issue['parent']>(null)
  const [another, setAnother] = useState(false)
  const [busy, setBusy] = useState(false)
  // the type's template fills an untouched description (and follows the type while untouched)
  const [applied, setApplied] = useState<string | null>(null)
  useEffect(() => {
    if (!type) return
    const untouched = desc === null || isEmptyDoc(desc) || (applied !== null && JSON.stringify(desc) === applied)
    if (!untouched) return
    const next = type.template ?? null
    setDesc(next)
    setApplied(next ? JSON.stringify(next) : null)
    setDescKey((k) => k + 1)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [type?.id])

  const submit = async () => {
    if (!project || !title.trim() || !type) return
    setBusy(true)
    try {
      const body: Record<string, unknown> = {
        projectKey: project.key,
        title,
        typeId: type.id,
        priority,
        labels,
        assigneeId: assignee?.id ?? defaults?.assigneeId ?? null,
        dueDate: due,
        startDate: defaults?.startDate,
        description: desc && !isEmptyDoc(desc) ? desc : null,
      }
      if (templateId) body.templateId = templateId
      if (parent) body.parentId = parent.id
      else if (defaults?.parentId) body.parentId = defaults.parentId
      if (defaults?.sprintId !== undefined && project.kind === 'scrum') body.sprintId = defaults.sprintId
      const created = await api.post<Issue>('/issues', body)
      if (defaults?.statusId && defaults.statusId !== created.status.id) await api.patch(`/issues/${created.key}`, { statusId: defaults.statusId })
      try {
        localStorage.setItem(LAST_PROJECT, project.key)
      } catch {
        /* ignore */
      }
      refreshIssues(qc)
      onCreated?.(created)
      if (another) {
        setTitle('')
        toast(t('Created {key}', { key: created.key }))
      } else {
        toast(t('Created {key}', { key: created.key }), {
          label: t('Open [[action]]'),
          run: () => {
            const url = new URL(window.location.href)
            url.searchParams.set('issue', created.key)
            window.history.pushState({}, '', url)
            window.dispatchEvent(new PopStateEvent('popstate'))
          },
        })
        onClose()
      }
    } catch (e) {
      toast((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      title={t('Create issue')}
      onClose={onClose}
      width={720}
      footer={
        <>
          <label className="row small muted" style={{ marginInlineEnd: 'auto' }}>
            <input type="checkbox" checked={another} onChange={(e) => setAnother(e.target.checked)} /> {t('Create another')}
          </label>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !title.trim() || !project} onClick={submit}>{t('Create')}</button>
        </>
      }
    >
      {(templates.data ?? []).length > 0 && (
        <div className="field" style={{ maxWidth: 360 }}>
          <label htmlFor="ci-template">{t('Template')}</label>
          <select
            id="ci-template"
            className="select"
            value={templateId}
            onChange={(e) => {
              const tp = templates.data?.find((x) => x.id === e.target.value)
              setTemplateId(tp?.id ?? '')
              if (!tp) return
              if (tp.fields.typeId) setTypeId(tp.fields.typeId)
              if (tp.title) setTitle(fillTitle(tp.title))
              if (tp.fields.priority) setPriority(tp.fields.priority)
              setLabels(tp.fields.labels ?? [])
              if (tp.description) {
                setDesc(tp.description)
                setApplied(null) // counts as written: changing the type keeps it
                setDescKey((k) => k + 1)
              }
            }}
          >
            <option value="">{t('No template')}</option>
            {templates.data?.map((tp) => <option key={tp.id} value={tp.id}>{tp.name}</option>)}
          </select>
          {templateId && (templates.data?.find((x) => x.id === templateId)?.subtasks.length ?? 0) > 0 && (
            <span className="field-hint">{t('Its {count} sub-tasks are created too.', { count: templates.data?.find((x) => x.id === templateId)?.subtasks.length ?? 0 })}</span>
          )}
        </div>
      )}
      <div className="row-wrap" style={{ marginBottom: 12 }}>
        <div className="field" style={{ margin: 0, minWidth: 200 }}>
          <label htmlFor="ci-project">{t('Project')}</label>
          <select id="ci-project" className="select" value={projectKey ?? ''} onChange={(e) => { setProjectKey(e.target.value); setTypeId(undefined); setParent(null) }}>
            {writable.map((p) => <option key={p.id} value={p.key}>{p.name} ({p.key})</option>)}
          </select>
        </div>
        <div className="field" style={{ margin: 0, minWidth: 160 }}>
          <label htmlFor="ci-type">{t('Type')}</label>
          <div className="row" style={{ gap: 6 }}>
            {type && <TypeIcon icon={type.icon} color={type.color} size={20} />}
            <select id="ci-type" className="select" value={type?.id ?? ''} onChange={(e) => setTypeId(e.target.value)}>
              {project?.types.map((x) => <option key={x.id} value={x.id}>{x.name}</option>)}
            </select>
          </div>
        </div>
      </div>
      <div className="field">
        <label htmlFor="ci-title">{t('Title')}</label>
        <input
          id="ci-title"
          className="input"
          autoFocus
          value={title}
          placeholder={t('A short summary of the work')}
          onChange={(e) => setTitle(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey || !e.shiftKey)) {
              e.preventDefault()
              void submit()
            }
          }}
        />
      </div>
      <div className="field">
        <span className="field-label">{t('Description')}</span>
        <RichText key={`${descKey}-${projectKey}`} boxed value={desc ?? EMPTY_DOC} onChange={setDesc} placeholder={t('Context, acceptance criteria… @ mentions, # issues')} />
      </div>
      {project && (
        <div className="row-wrap" style={{ gap: 16 }}>
          <div className="field" style={{ minWidth: 200 }}>
            <span className="field-label">{t('Assignee')}</span>
            <PersonPicker value={assignee} onChange={(id) => setAssignee(id ? { id, name: '' } : null)} />
          </div>
          <div className="field">
            <span className="field-label">{t('Priority')}</span>
            <PriorityPicker value={priority} onChange={setPriority} />
          </div>
          <div className="field">
            <span className="field-label">{t('Labels')}</span>
            <LabelsEditor value={labels} onChange={setLabels} />
          </div>
          <div className="field">
            <span className="field-label">{t('Due')}</span>
            <DateValue value={due} onChange={setDue} />
          </div>
          {type && type.level <= 0 && !defaults?.parentId && (
            <div className="field" style={{ minWidth: 220 }}>
              <span className="field-label">{type.level < 0 ? t('Parent') : t('Epic')}</span>
              <ParentPicker value={parent} projectKey={project.key} epicsOnly={type.level === 0} onChange={async (id) => {
                if (!id) return setParent(null)
                const p = await api.get<Issue>(`/issues/${id}`)
                setParent({ id: p.id, key: p.key, title: p.title, color: p.type.color })
              }} />
            </div>
          )}
        </div>
      )}
      <AssigneeName assignee={assignee} setAssignee={setAssignee} />
    </Modal>
  )
}

/** PersonPicker reports ids; keep the name for the button. */
function AssigneeName({ assignee, setAssignee }: { assignee: { id: string; name: string } | null; setAssignee: (a: { id: string; name: string } | null) => void }) {
  const people = useQueryClient().getQueryData<{ id: string; name: string }[]>(['people'])
  useEffect(() => {
    if (assignee && !assignee.name) {
      const p = people?.find((x) => x.id === assignee.id)
      if (p) setAssignee({ id: p.id, name: p.name })
    }
  }, [assignee, people, setAssignee])
  return null
}
