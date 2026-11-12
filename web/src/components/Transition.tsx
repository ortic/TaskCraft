// Moving an issue into a status with rules: before the move, a small form asks for what the
// status needs — the fields it requires that are still empty, a comment, a resolution. Callers
// use `useTransition()(issue, project, statusId)`: it resolves to the extra fields to send with
// the move ({} when there's nothing to ask), or null when the person cancelled.
import { t } from '../i18n'
import { createContext, useCallback, useContext, useState, type ReactNode } from 'react'
import type { Issue, Project, Status, StatusRules } from '../api'
import { Modal, usePeople } from '../ui'
import { ComponentsPicker, CustomFieldValue, DateValue, LabelsEditor, NumberValue, PersonPicker, VersionsPicker } from './fields'

export type { StatusRules }

/** The resolutions of projects that didn't set their own (as the server stores them). */
export const DEFAULT_RESOLUTIONS = ['Done', "Won't do", 'Duplicate', 'Cannot reproduce']

/** The project's resolutions (its own list, else the defaults). */
export function resolutionsOf(project: Project): string[] {
  const own = (project.settings as Record<string, unknown> | undefined)?.resolutions
  return Array.isArray(own) && own.length ? (own as string[]) : DEFAULT_RESOLUTIONS
}

/** The built-in resolutions in the reader's language; a project's own names as they are. */
export function resolutionLabel(name: string): string {
  switch (name) {
    case 'Done':
      return t('Done [[resolution]]')
    case "Won't do":
      return t('Won’t do')
    case 'Duplicate':
      return t('Duplicate [[resolution]]')
    case 'Cannot reproduce':
      return t('Cannot reproduce')
    default:
      return name
  }
}

/** What a requirement is called in the form. */
export function requirementLabel(key: string, project: Project): string {
  switch (key) {
    case 'assignee':
      return t('Assignee')
    case 'resolution':
      return t('Resolution')
    case 'version':
      return t('Release')
    case 'affects':
      return t('Found in')
    case 'component':
      return t('Component')
    case 'labels':
      return t('Labels')
    case 'priority':
      return t('Priority')
    case 'sprint':
      return t('Sprint')
    case 'security':
      return t('Security')
    case 'dueDate':
      return t('Due date')
    case 'startDate':
      return t('Start date')
    case 'estimate':
      return t('Estimate (hours)')
    case 'storyPoints':
      return t('Story points')
    default:
      return project.fields.find((f) => `custom:${f.id}` === key)?.name ?? key
  }
}

function isEmpty(v: unknown) {
  return v == null || v === '' || (Array.isArray(v) && v.length === 0)
}

/** The required fields still empty on `issue` (all of them when there's no issue: a bulk change). */
function missing(issue: Issue | null, rules: StatusRules): string[] {
  return (rules.require ?? []).filter((k) => {
    if (k === 'resolution') return false
    if (!issue) return true
    switch (k) {
      case 'assignee':
        return !issue.assignee
      case 'version':
        return !(issue.versions ?? []).length
      case 'affects':
        return !(issue.affectedVersions ?? []).length
      case 'labels':
        return !issue.labels.length
      case 'component':
        return !(issue.components ?? []).length
      case 'dueDate':
        return !issue.dueDate
      case 'startDate':
        return !issue.startDate
      case 'estimate':
        return issue.estimateMinutes == null
      case 'storyPoints':
        return issue.storyPoints == null
      default:
        return k.startsWith('custom:') ? isEmpty(issue.custom?.[k.slice(7)]) : false
    }
  })
}

/** An input for a field a status or an issue type asks for (values as `toFields` reads them). */
export function FieldInput({ k, project, value, onChange }: { k: string; project: Project; value: unknown; onChange: (v: unknown) => void }) {
  const people = usePeople().data ?? []
  const ids = (value as string[] | undefined) ?? []
  const field = k.startsWith('custom:') ? project.fields.find((f) => f.id === k.slice(7)) : undefined
  switch (k) {
    case 'assignee':
      return <PersonPicker value={people.find((p) => p.id === value) ?? null} label={requirementLabel(k, project)} onChange={(id) => onChange(id)} allowNone={false} />
    case 'version':
    case 'affects':
      return <VersionsPicker kind={k === 'version' ? 'fix' : 'affects'} project={project} value={project.versions.filter((v) => ids.includes(v.id))} onChange={onChange} />
    case 'component':
      return <ComponentsPicker project={project} value={project.components.filter((c) => ids.includes(c.id))} onChange={onChange} />
    case 'labels':
      return <LabelsEditor value={ids} onChange={onChange} />
    case 'dueDate':
    case 'startDate':
      return <DateValue value={(value as string) ?? null} onChange={onChange} />
    case 'estimate':
      return <NumberValue value={(value as number) ?? null} onChange={onChange} suffix={t('hours')} />
    case 'storyPoints':
      return <NumberValue value={(value as number) ?? null} onChange={onChange} />
    default:
      return field ? <CustomFieldValue field={field} value={value ?? null} onChange={onChange} /> : null
  }
}

/** The values of `FieldInput`s as issue fields to send. */
export function toFields(values: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {}
  const custom: Record<string, unknown> = {}
  for (const [k, v] of Object.entries(values)) {
    if (k.startsWith('custom:')) custom[k.slice(7)] = v
    else if (k === 'assignee') out.assigneeId = v
    else if (k === 'version') out.versionIds = v
    else if (k === 'affects') out.affectedVersionIds = v
    else if (k === 'component') out.componentIds = v
    else if (k === 'estimate') out.estimateMinutes = v == null ? null : Math.round(Number(v) * 60)
    else out[k] = v
  }
  if (Object.keys(custom).length) out.custom = custom
  return out
}

/** Whether a `FieldInput` value counts as filled in. */
export function isFilled(v: unknown) {
  return !isEmpty(v)
}

type Ask = (issue: Issue | null, project: Project, statusId: string) => Promise<Record<string, unknown> | null>
const Ctx = createContext<Ask>(async () => ({}))

interface Pending {
  issue: Issue | null
  project: Project
  status: Status
  rules: StatusRules
  fields: string[]
  resolve: (v: Record<string, unknown> | null) => void
}

export function TransitionProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<Pending | null>(null)
  const ask = useCallback<Ask>((issue, project, statusId) => {
    const status: Status | undefined = project.statuses.find((s) => s.id === statusId)
    const rules = status?.rules ?? {}
    const fields = missing(issue, rules)
    const done = status?.category === 'done'
    const needsResolution = done && (rules.require ?? []).includes('resolution')
    const asksComment = rules.comment === 'optional' || rules.comment === 'required'
    if (!status || (!fields.length && !needsResolution && !asksComment)) return Promise.resolve({})
    return new Promise((resolve) => setPending({ issue, project, status, rules, fields, resolve }))
  }, [])
  return (
    <Ctx.Provider value={ask}>
      {children}
      {pending && (
        <TransitionForm
          {...pending}
          onDone={(v) => {
            pending.resolve(v)
            setPending(null)
          }}
        />
      )}
    </Ctx.Provider>
  )
}

export function useTransition(): Ask {
  return useContext(Ctx)
}

function TransitionForm({ issue, project, status, rules, fields, onDone }: Pending & { onDone: (v: Record<string, unknown> | null) => void }) {
  const done = status.category === 'done'
  const names = resolutionsOf(project)
  const [resolution, setResolution] = useState(() => names.find((n) => n.toLowerCase() === rules.resolution?.toLowerCase()) ?? names[0])
  const [comment, setComment] = useState('')
  const [values, setValues] = useState<Record<string, unknown>>({})
  const [error, setError] = useState('')
  const set = (k: string, v: unknown) => setValues((o) => ({ ...o, [k]: v }))
  const submit = () => {
    const empty = fields.filter((k) => isEmpty(values[k])).map((k) => requirementLabel(k, project))
    if (rules.comment === 'required' && !comment.trim()) empty.push(t('Comment'))
    if (empty.length) {
      setError(t('Still needed: {fields}', { fields: empty.join(', ') }))
      return
    }
    const out = toFields(Object.fromEntries(fields.map((k) => [k, values[k]])))
    if (done) out.resolution = resolution
    if (comment.trim()) out.comment = comment.trim()
    onDone(out)
  }
  const title = issue ? t('Move {key} to {status}', { key: issue.key, status: status.name }) : t('Move to {status}', { status: status.name })
  return (
    <Modal
      title={title}
      onClose={() => onDone(null)}
      width={480}
      footer={
        <>
          <button className="btn" onClick={() => onDone(null)}>{t('Cancel')}</button>
          <button className="btn btn-primary" onClick={submit}>{t('Move')}</button>
        </>
      }
    >
      <form
        className="tr-form"
        onSubmit={(e) => {
          e.preventDefault()
          submit()
        }}
      >
        {fields.length > 0 && <p className="muted small" style={{ marginTop: 0 }}>{t('{status} needs these filled in:', { status: status.name })}</p>}
        {fields.map((k) => {
          return (
            <div className="field" key={k}>
              <span className="field-label">{requirementLabel(k, project)} <span className="tr-req">{t('required')}</span></span>
              <FieldInput k={k} project={project} value={values[k]} onChange={(v) => set(k, v)} />
            </div>
          )
        })}
        {done && (
          <div className="field">
            <label htmlFor="tr-resolution">{t('Resolution')}</label>
            <select id="tr-resolution" className="select" value={resolution} onChange={(e) => setResolution(e.target.value)}>
              {names.map((n) => <option key={n} value={n}>{resolutionLabel(n)}</option>)}
            </select>
          </div>
        )}
        {(rules.comment === 'optional' || rules.comment === 'required') && (
          <div className="field">
            <label htmlFor="tr-comment">{t('Comment')} {rules.comment === 'required' ? <span className="tr-req">{t('required')}</span> : <span className="faint">{t('(optional)')}</span>}</label>
            <textarea id="tr-comment" className="input" rows={4} value={comment} onChange={(e) => setComment(e.target.value)} placeholder={t('Why it moves, what’s next…')} />
          </div>
        )}
        {error && <p className="error-text" role="alert">{error}</p>}
      </form>
    </Modal>
  )
}
