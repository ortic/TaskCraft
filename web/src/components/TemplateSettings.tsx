// Project settings: issue templates (optionally recurring) and service-level goals.
import { t } from '../i18n'
import { useState, type ReactNode } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Pencil, Trash, Repeat, FileText, Play } from 'lucide-react'
import { api, dateTime, PRIORITIES, type Issue, type Priority, type Project } from '../api'
import { Confirm, Empty, ErrorBox, Loading, Modal, useToast } from '../ui'
import { TypeIcon, priorityLabel } from '../icons'
import ScheduleEditor, { defaultSchedule, describeSchedule, type Schedule } from './ScheduleEditor'
import { LabelsEditor, PersonPicker } from './fields'
import { refreshIssues } from './IssueView'

export interface Template {
  id: string
  name: string
  title: string
  description: any
  fields: { typeId?: string; priority?: Priority; labels?: string[]; assigneeId?: string | null; storyPoints?: number | null; dueInDays?: number | null; sprint?: 'active' | null }
  subtasks: string[]
  schedule: Schedule | null
  nextRunAt: string | null
  lastRunAt: string | null
  lastIssueKey: string | null
}

function Block({ title, description, actions, children }: { title: ReactNode; description?: ReactNode; actions?: ReactNode; children: ReactNode }) {
  return (
    <section className="ps-section">
      <div className="ps-section-head">
        <div className="ps-section-text">
          <h2>{title}</h2>
          {description && <p className="ps-desc">{description}</p>}
        </div>
        {actions && <div className="ps-section-actions">{actions}</div>}
      </div>
      {children}
    </section>
  )
}

/** Rich text → simple Markdown for editing in a textarea. */
function docText(doc: any): string {
  if (!doc?.content) return ''
  const inline = (n: any): string =>
    (n.content ?? [])
      .map((c: any) => {
        if (c.type === 'text') {
          let x = c.text ?? ''
          const m = (c.marks ?? []).map((k: any) => k.type)
          if (m.includes('code')) x = '`' + x + '`'
          if (m.includes('bold')) x = `**${x}**`
          if (m.includes('italic')) x = `*${x}*`
          return x
        }
        return c.type === 'hardBreak' ? '\n' : ''
      })
      .join('')
  const block = (n: any, prefix = ''): string => {
    switch (n.type) {
      case 'heading':
        return '#'.repeat(n.attrs?.level ?? 2) + ' ' + inline(n)
      case 'bulletList':
        return (n.content ?? []).map((li: any) => prefix + '- ' + (li.content ?? []).map((c: any) => block(c)).join('\n')).join('\n')
      case 'orderedList':
        return (n.content ?? []).map((li: any, i: number) => `${prefix}${i + 1}. ` + (li.content ?? []).map((c: any) => block(c)).join('\n')).join('\n')
      case 'taskList':
        return (n.content ?? []).map((li: any) => `${prefix}- [${li.attrs?.checked ? 'x' : ' '}] ` + (li.content ?? []).map((c: any) => block(c)).join('\n')).join('\n')
      case 'codeBlock':
        return '```\n' + inline(n) + '\n```'
      case 'blockquote':
        return (n.content ?? []).map((c: any) => '> ' + block(c)).join('\n')
      default:
        return inline(n)
    }
  }
  return doc.content.map((n: any) => block(n)).join('\n\n').trim()
}

export function TemplatesSection({ project }: { project: Project }) {
  const qc = useQueryClient()
  const toast = useToast()
  const qk = ['templates', project.key]
  const list = useQuery({ queryKey: qk, queryFn: () => api.get<Template[]>(`/projects/${project.key}/templates`) })
  const [editing, setEditing] = useState<Template | 'new' | null>(null)
  const [deleting, setDeleting] = useState<Template | null>(null)
  return (
    <Block
      title={t('Templates')}
      description={t('Ready-made issues: title, description, fields and sub-tasks. People pick them when creating an issue; with a schedule, a template creates its issue by itself (a weekly report, a monthly check).')}
      actions={<button className="btn btn-primary" onClick={() => setEditing('new')}><Plus size={15} /> {t('New template')}</button>}
    >
      {list.isLoading && <Loading />}
      {list.error && <ErrorBox error={list.error} />}
      {list.data?.length === 0 && (
        <Empty icon={<FileText size={32} />} title={t('No templates yet')}>
          {t('For example: a release checklist with its sub-tasks, or a recurring “Weekly report {week}”.', { week: '{week}' })}
        </Empty>
      )}
      <div className="ps-rules">
        {list.data?.map((tp) => {
          const type = project.types.find((x) => x.id === tp.fields.typeId)
          return (
            <div key={tp.id} className="ps-rule">
              {type ? <TypeIcon icon={type.icon} color={type.color} size={20} /> : <FileText size={18} />}
              <div className="ps-rule-main">
                <button className="ps-link-btn ps-strong" onClick={() => setEditing(tp)}>{tp.name}</button>
                <div className="ps-rule-sentence">
                  {tp.title || t('(title chosen when creating)')}
                  {tp.subtasks.length > 0 && <span className="faint"> · {t('{count} sub-tasks', { count: tp.subtasks.length })}</span>}
                </div>
                {tp.schedule && (
                  <div className="faint small row" style={{ gap: 4 }}>
                    <Repeat size={12} /> {describeSchedule(tp.schedule)}
                    {tp.nextRunAt && <> · {t('next {when}', { when: dateTime(tp.nextRunAt) })}</>}
                    {tp.lastIssueKey && <> · {t('last created {key}', { key: tp.lastIssueKey })}</>}
                  </div>
                )}
              </div>
              <button
                className="icon-btn"
                title={t('Create an issue from it now')}
                aria-label={t('Create an issue from {name} now', { name: tp.name })}
                onClick={async () => {
                  try {
                    const made = await api.post<Issue>(`/templates/${tp.id}/create`, {})
                    refreshIssues(qc)
                    toast(t('Created {key}', { key: made.key }))
                  } catch (e) {
                    toast((e as Error).message)
                  }
                }}
              >
                <Play size={15} />
              </button>
              <button className="icon-btn" title={t('Edit')} aria-label={t('Edit {name}', { name: tp.name })} onClick={() => setEditing(tp)}><Pencil size={15} /></button>
              <button className="icon-btn" title={t('Delete')} aria-label={t('Delete {name}', { name: tp.name })} onClick={() => setDeleting(tp)}><Trash size={15} /></button>
            </div>
          )
        })}
      </div>
      {editing && <TemplateDialog project={project} template={editing === 'new' ? null : editing} onClose={() => setEditing(null)} />}
      {deleting && (
        <Confirm
          title={t('Delete template {name}?', { name: deleting.name })}
          body={<p>{t('Issues already created from it stay.')}</p>}
          confirmLabel={t('Delete')}
          danger
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            await api.del(`/templates/${deleting.id}`)
            qc.invalidateQueries({ queryKey: qk })
          }}
        />
      )}
    </Block>
  )
}

function TemplateDialog({ project, template, onClose }: { project: Project; template: Template | null; onClose: () => void }) {
  const qc = useQueryClient()
  const [name, setName] = useState(template?.name ?? '')
  const [title, setTitle] = useState(template?.title ?? '')
  const [description, setDescription] = useState(docText(template?.description))
  const [typeId, setTypeId] = useState(template?.fields.typeId ?? project.types.find((x) => x.level === 0)?.id ?? '')
  const [priority, setPriority] = useState<Priority>(template?.fields.priority ?? 'medium')
  const [labels, setLabels] = useState<string[]>(template?.fields.labels ?? [])
  const [assignee, setAssignee] = useState<{ id: string; name: string } | null>(template?.fields.assigneeId ? { id: template.fields.assigneeId, name: '' } : null)
  const [dueInDays, setDueInDays] = useState<string>(template?.fields.dueInDays != null ? String(template.fields.dueInDays) : '')
  const [activeSprint, setActiveSprint] = useState(template?.fields.sprint === 'active')
  const [subtasks, setSubtasks] = useState((template?.subtasks ?? []).join('\n'))
  const [recurring, setRecurring] = useState(!!template?.schedule)
  const [schedule, setSchedule] = useState<Schedule>(template?.schedule ?? defaultSchedule())
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const isSubtask = project.types.find((x) => x.id === typeId)?.level === -1
  return (
    <Modal
      title={template ? t('Edit template') : t('New template')}
      onClose={onClose}
      width={680}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button
            className="btn btn-primary"
            disabled={busy || !name.trim() || (recurring && !title.trim())}
            onClick={async () => {
              setBusy(true)
              setErr('')
              const fields: Record<string, unknown> = { typeId, priority, labels }
              if (assignee) fields.assigneeId = assignee.id
              if (dueInDays.trim()) fields.dueInDays = Number(dueInDays)
              if (activeSprint) fields.sprint = 'active'
              const body = {
                name,
                title,
                description,
                fields,
                subtasks: isSubtask ? [] : subtasks.split('\n').map((s) => s.trim()).filter(Boolean),
                schedule: recurring ? schedule : null,
              }
              try {
                if (template) await api.patch(`/templates/${template.id}`, body)
                else await api.post(`/projects/${project.key}/templates`, body)
                qc.invalidateQueries({ queryKey: ['templates', project.key] })
                onClose()
              } catch (e) {
                setErr((e as Error).message)
              } finally {
                setBusy(false)
              }
            }}
          >{t('Save template')}</button>
        </>
      }
    >
      <div className="row" style={{ gap: 12, alignItems: 'flex-start' }}>
        <div className="field" style={{ flex: 1 }}>
          <label htmlFor="tp-name">{t('Template name')}</label>
          <input id="tp-name" className="input" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder={t('e.g. Release checklist')} />
        </div>
        <div className="field" style={{ width: 180 }}>
          <label htmlFor="tp-type">{t('Type')}</label>
          <select id="tp-type" className="select" value={typeId} onChange={(e) => setTypeId(e.target.value)}>
            {project.types.map((x) => <option key={x.id} value={x.id}>{x.name}</option>)}
          </select>
        </div>
      </div>
      <div className="field">
        <label htmlFor="tp-title">{t('Issue title')}</label>
        <input id="tp-title" className="input" value={title} onChange={(e) => setTitle(e.target.value)} placeholder={t('e.g. Weekly report {week}', { week: '{week}' })} />
        <span className="field-hint">{t('{date}, {week} and {month} are filled in when an issue is created.', { date: '{date}', week: '{week}', month: '{month}' })}</span>
      </div>
      <div className="field">
        <label htmlFor="tp-desc">{t('Description (Markdown)')}</label>
        <textarea id="tp-desc" className="textarea" rows={5} value={description} onChange={(e) => setDescription(e.target.value)} />
      </div>
      <div className="row-wrap" style={{ gap: 16, alignItems: 'flex-start' }}>
        <div className="field">
          <label htmlFor="tp-prio">{t('Priority')}</label>
          <select id="tp-prio" className="select" style={{ width: 'auto' }} value={priority} onChange={(e) => setPriority(e.target.value as Priority)}>
            {PRIORITIES.map((p) => <option key={p} value={p}>{priorityLabel(p)}</option>)}
          </select>
        </div>
        <div className="field" style={{ minWidth: 200 }}>
          <span className="field-label">{t('Assignee')}</span>
          <PersonPicker value={assignee} onChange={(id) => setAssignee(id ? { id, name: '' } : null)} />
        </div>
        <div className="field">
          <span className="field-label">{t('Labels')}</span>
          <LabelsEditor value={labels} onChange={setLabels} />
        </div>
        <div className="field" style={{ width: 120 }}>
          <label htmlFor="tp-due">{t('Due after (days)')}</label>
          <input id="tp-due" className="input" inputMode="numeric" value={dueInDays} onChange={(e) => setDueInDays(e.target.value.replace(/[^0-9]/g, ''))} />
        </div>
      </div>
      {project.kind === 'scrum' && (
        <label className="row small" style={{ marginBottom: 10 }}>
          <input type="checkbox" checked={activeSprint} onChange={(e) => setActiveSprint(e.target.checked)} /> {t('Put it into the running sprint')}
        </label>
      )}
      {!isSubtask && (
        <div className="field">
          <label htmlFor="tp-subs">{t('Sub-tasks (one per line)')}</label>
          <textarea id="tp-subs" className="textarea" rows={3} value={subtasks} onChange={(e) => setSubtasks(e.target.value)} />
        </div>
      )}
      <div className="card" style={{ padding: 12 }}>
        <label className="row" style={{ fontWeight: 500 }}>
          <input type="checkbox" checked={recurring} onChange={(e) => setRecurring(e.target.checked)} /> <Repeat size={14} /> {t('Create an issue from it on a schedule')}
        </label>
        {recurring && (
          <div style={{ marginTop: 8 }}>
            <ScheduleEditor value={schedule} onChange={setSchedule} />
            <p className="faint small" style={{ marginBottom: 0 }}>{t('Scheduled issues are reported by you (whoever saved the template last).')}</p>
          </div>
        )}
      </div>
      {err && <p className="error-text">{err}</p>}
    </Modal>
  )
}

// ---- service levels ----

const GOALS: { id: 'response' | 'resolution'; label: () => string; hint: () => string }[] = [
  { id: 'response', label: () => t('First response'), hint: () => t('until someone other than the reporter comments, or the issue leaves “to do”') },
  { id: 'resolution', label: () => t('Resolution'), hint: () => t('until the issue is done') },
]

export function ServiceLevelsSection({ project, onSaved }: { project: Project; onSaved: () => void }) {
  const toast = useToast()
  const current = (project.settings.sla ?? {}) as Record<string, Record<string, number | null>>
  const [values, setValues] = useState<Record<string, Record<string, string>>>(() => {
    const v: Record<string, Record<string, string>> = {}
    for (const g of GOALS) {
      v[g.id] = {}
      for (const p of PRIORITIES) v[g.id][p] = current[g.id]?.[p] != null ? String(current[g.id][p]) : ''
    }
    return v
  })
  const [busy, setBusy] = useState(false)
  const save = async (clear?: boolean) => {
    setBusy(true)
    const sla: Record<string, Record<string, number>> = {}
    if (!clear)
      for (const g of GOALS)
        for (const p of PRIORITIES) {
          const n = Number(values[g.id][p].replace(',', '.'))
          if (values[g.id][p].trim() && n > 0) (sla[g.id] ??= {})[p] = n
        }
    try {
      await api.patch(`/projects/${project.key}`, { settings: { sla: Object.keys(sla).length ? sla : null } })
      toast(t('Service levels saved'))
      onSaved()
    } catch (e) {
      toast((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Block
      title={t('Service levels')}
      description={t('Goals in hours, per priority: how soon issues get a first response and how soon they are resolved. Issues show how much time is left, the queue lists the most urgent first, and whoever is assigned (else the project lead) is told when a goal is missed.')}
    >
      <table className="list" style={{ maxWidth: 640 }}>
        <thead>
          <tr>
            <th>{t('Priority')}</th>
            {GOALS.map((g) => <th key={g.id} title={g.hint()}>{g.label()} ({t('hours')})</th>)}
          </tr>
        </thead>
        <tbody>
          {PRIORITIES.map((p) => (
            <tr key={p}>
              <td>{priorityLabel(p)}</td>
              {GOALS.map((g) => (
                <td key={g.id}>
                  <input
                    className="input input-sm"
                    style={{ width: 100 }}
                    inputMode="decimal"
                    aria-label={`${g.label()} · ${priorityLabel(p)}`}
                    placeholder={t('no goal')}
                    value={values[g.id][p]}
                    onChange={(e) => setValues((v) => ({ ...v, [g.id]: { ...v[g.id], [p]: e.target.value } }))}
                  />
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      <p className="faint small">{GOALS.map((g) => `${g.label()}: ${g.hint()}`).join(' · ')}</p>
      <div className="row">
        <button className="btn btn-primary" disabled={busy} onClick={() => save()}>{t('Save')}</button>
        {project.settings.sla && <button className="btn btn-ghost" disabled={busy} onClick={() => save(true)}>{t('Turn service levels off')}</button>}
      </div>
    </Block>
  )
}
