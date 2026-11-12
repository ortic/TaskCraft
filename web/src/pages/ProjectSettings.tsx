// Project settings for project admins: details, members, workflow, issue types, fields and
// components, boards, automation rules, integrations and the trash.
import { t } from '../i18n'
import ScheduleEditor, { defaultSchedule, describeSchedule, type Schedule } from '../components/ScheduleEditor'
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Link, NavLink, useNavigate, useParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import {
  SlidersHorizontal, Users, Workflow, Shapes, TextCursorInput, Kanban, Bot, Plug, Trash, Shield, GripVertical, Plus, Pencil, X, Check, ArrowRight,
  Archive, ArchiveArrowUp, TriangleExclamationPoint, Type, Hash, CircleDot, ListChecks, Calendar, User, SquareCheck, Link as LinkIcon, Puzzle, Search,
  ExternalLink, BookOpen, Undo2, Info, Lock, LockOpen, Columns3, Funnel, ChevronDown, ChevronRight, FileStack, Timer, Upload, Mail, LifeBuoy,
} from 'lucide-react'
import {
  api, ago, dateTime, PRIORITIES, type Category, type CustomField, type FieldOption, type IssueType, type Named, type Priority, type Project, type Role,
  type BoardData, type BoardConfig, type BoardColumn, type Status, type WikiPage,
} from '../api'
import { Avatar, Choices, Confirm, Empty, ErrorBox, Loading, Lozenge, Modal, PopButton, useMe, usePeople, useProject, useToast } from '../ui'
import { PROJECT_COLORS, PROJECT_ICONS, ProjectIcon, TYPE_ICONS, TypeIcon, priorityLabel } from '../icons'
import { OptionChip, PersonPicker } from '../components/fields'
import { useOpenIssue } from '../components/Layout'
import { ServiceLevelsSection, TemplatesSection } from '../components/TemplateSettings'
import { ImportSection } from '../components/ImportSettings'
import GitSettings from './GitSettings'
import MailInSettings from './MailInSettings'
import PortalSettings from './PortalSettings'
import './ProjectSettings.css'

// ---- local types (server shapes) ----

type Grant = { subjectType: 'user' | 'team'; subjectId: string; role: Role; name: string | null }
type MembersData = { defaultRole: Role; grants: Grant[]; people: { id: string; name: string; role: Role }[] }
type Team = { id: string; name: string; memberIds: string[]; color?: string | null }
type TriggerEvent = 'created' | 'status' | 'assigned' | 'field' | 'commented' | 'schedule'
type Trigger = { event: TriggerEvent; toStatusId?: string; fromStatusId?: string; toCategory?: Category; field?: string; schedule?: Schedule }
type ActionKind = 'setStatus' | 'assign' | 'setPriority' | 'addLabel' | 'removeLabel' | 'setSprint' | 'comment' | 'notify'
type NotifyTo = 'assignee' | 'reporter' | 'watchers' | 'lead'
type RuleAction = { action: ActionKind; statusId?: string; to?: 'reporter' | 'nobody' | NotifyTo; userId?: string; priority?: Priority; label?: string; sprint?: 'active' | null; text?: string; message?: string }
type Rule = { id: string; name: string; enabled: boolean; trigger: Trigger; condition: string; actions: RuleAction[]; runs: number; lastRunAt: string | null; nextRunAt?: string | null }
type TrashItem = { id: string; key: string; title: string; deletedAt: string | null; deletedBy: string | null }
type WikiSpace = { key: string; name: string; url: string }

// ---- helpers ----

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e))

const CATEGORIES: Category[] = ['todo', 'in_progress', 'done']
const catLabel = (c: Category) => (c === 'todo' ? t('To do') : c === 'in_progress' ? t('In progress') : t('Done'))
const catHint = (c: Category) =>
  c === 'todo' ? t('Not started yet') : c === 'in_progress' ? t('Being worked on (counts for cycle time)') : t('Finished (counts as resolved)')

const levelLabel = (l: number) => (l > 0 ? t('Epic') : l < 0 ? t('Sub-task') : t('Standard'))
const levelHint = (l: number) =>
  l > 0 ? t('Groups standard issues; shown in the epic panel and on the timeline') : l < 0 ? t('Lives inside a standard issue') : t('Stories, tasks, bugs: the work on boards')

const roleLabel = (r: Role) => (r === 'admin' ? t('Admin') : r === 'member' ? t('Member') : r === 'viewer' ? t('Viewer') : t('No access'))
const roleHint = (r: Role) =>
  r === 'admin'
    ? t('Everything members can, plus settings, workflow, boards and members')
    : r === 'member'
      ? t('Create and edit issues, run sprints, manage releases')
      : r === 'viewer'
        ? t('See issues, comment and watch')
        : t('Only people and teams added under Members')

const COLORS = [...new Set([...PROJECT_COLORS, '#8b8d98', '#5b8def', '#3b82f6', '#f59e0b', '#a855f7', '#22a06b', '#e5484d', '#5c6370', '#64748b'])]

let idSeq = 0
/** A fresh id for things the server stores as given (board columns). */
function newId(): string {
  try {
    if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) return crypto.randomUUID()
  } catch {
    /* insecure context */
  }
  return `c${Date.now().toString(36)}${(idSeq++).toString(36)}${Math.random().toString(36).slice(2, 8)}`
}

/** A short random id for a new select option (the server would give options created together the same id). */
function optionId(): string {
  const bytes = new Uint8Array(6)
  try {
    crypto.getRandomValues(bytes)
  } catch {
    bytes.forEach((_, i) => (bytes[i] = Math.floor(Math.random() * 256)))
  }
  return [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('')
}

/** A rich-text document (the server's JSON doc) as Markdown for the template editor. */
function docToMarkdown(doc: any): string {
  if (!doc) return ''
  if (typeof doc === 'string') return doc
  const inline = (nodes: any[] = []): string =>
    nodes
      .map((n) => {
        if (n.type === 'hardBreak') return '  \n'
        if (n.type === 'mention') return `@${n.attrs?.label ?? n.attrs?.id ?? ''}`
        if (n.type !== 'text') return inline(n.content)
        let s: string = n.text ?? ''
        for (const m of n.marks ?? []) {
          if (m.type === 'code') s = '`' + s + '`'
          else if (m.type === 'bold') s = `**${s}**`
          else if (m.type === 'italic') s = `*${s}*`
          else if (m.type === 'strike') s = `~~${s}~~`
          else if (m.type === 'link') s = `[${s}](${m.attrs?.href ?? ''})`
        }
        return s
      })
      .join('')
  const block = (n: any): string => {
    switch (n.type) {
      case 'heading':
        return '#'.repeat(Math.min(6, Math.max(1, n.attrs?.level ?? 1))) + ' ' + inline(n.content)
      case 'blockquote':
        return blocks(n.content)
          .split('\n')
          .map((l) => '> ' + l)
          .join('\n')
      case 'codeBlock':
        return '```' + (n.attrs?.language ?? '') + '\n' + inline(n.content) + '\n```'
      case 'horizontalRule':
        return '---'
      case 'bulletList':
      case 'orderedList':
      case 'taskList':
        return (n.content ?? [])
          .map((item: any, i: number) => {
            const marker = n.type === 'orderedList' ? `${(n.attrs?.start ?? 1) + i}. ` : item.type === 'taskItem' ? `- [${item.attrs?.checked ? 'x' : ' '}] ` : '- '
            const inner = (item.content ?? []).map(block).join('\n')
            return inner
              .split('\n')
              .map((l: string, j: number) => (j === 0 ? marker : ' '.repeat(marker.length)) + l)
              .join('\n')
          })
          .join('\n')
      default:
        return inline(n.content)
    }
  }
  const blocks = (nodes: any[] = []): string => nodes.map(block).join('\n\n')
  return blocks(doc.content).replace(/\n{3,}/g, '\n\n').replace(/^\n+|\n+$/g, '')
}

// ---- shared context ----

type Ctx = {
  project: Project
  /** refetch the project (and everything showing it) */
  refresh: () => void
  /** optimistic change of the cached project */
  patch: (fn: (p: Project) => Project) => void
  /** runs a change; toasts errors; refreshes on success */
  run: (fn: () => Promise<unknown>, ok?: string) => Promise<boolean>
}
const SettingsCtx = createContext<Ctx>(null as unknown as Ctx)
const useSettings = () => useContext(SettingsCtx)

const SECTIONS = [
  { id: 'details', icon: SlidersHorizontal, label: () => t('Details') },
  { id: 'members', icon: Users, label: () => t('Members') },
  { id: 'workflow', icon: Workflow, label: () => t('Workflow') },
  { id: 'types', icon: Shapes, label: () => t('Issue types') },
  { id: 'fields', icon: TextCursorInput, label: () => t('Fields & components') },
  { id: 'boards', icon: Kanban, label: () => t('Boards') },
  { id: 'templates', icon: FileStack, label: () => t('Templates') },
  { id: 'automation', icon: Bot, label: () => t('Automation') },
  { id: 'sla', icon: Timer, label: () => t('Service levels') },
  { id: 'email', icon: Mail, label: () => t('E-mail') },
  { id: 'service-desk', icon: LifeBuoy, label: () => t('Service desk') },
  { id: 'integrations', icon: Plug, label: () => t('Integrations') },
  { id: 'import', icon: Upload, label: () => t('Import') },
  { id: 'trash', icon: Trash, label: () => t('Trash') },
] as const

export default function ProjectSettings() {
  const params = useParams()
  const urlKey = params.key ?? ''
  const [tab = 'details', sub] = (params['*'] ?? '').split('/').filter(Boolean)
  const project = useProject(urlKey)
  const qc = useQueryClient()
  const toast = useToast()
  const cacheKey = useMemo(() => ['project', urlKey.toUpperCase()], [urlKey])
  const p = project.data

  const refresh = useCallback(() => {
    qc.invalidateQueries({ queryKey: cacheKey })
    if (p && p.key !== urlKey.toUpperCase()) qc.invalidateQueries({ queryKey: ['project', p.key] })
    qc.invalidateQueries({ queryKey: ['projects'] })
    qc.invalidateQueries({ queryKey: ['board'] })
  }, [qc, cacheKey, p, urlKey])
  const patch = useCallback((fn: (p: Project) => Project) => qc.setQueryData<Project>(cacheKey, (old) => (old ? fn(old) : old)), [qc, cacheKey])
  const run = useCallback(
    async (fn: () => Promise<unknown>, ok?: string) => {
      try {
        await fn()
        if (ok) toast(ok)
        return true
      } catch (e) {
        toast(errText(e))
        return false
      } finally {
        refresh()
      }
    },
    [refresh, toast],
  )
  const ctx = useMemo(() => (p ? { project: p, refresh, patch, run } : null), [p, refresh, patch, run])

  if (project.isLoading) return <Loading />
  if (project.error) return <div className="page"><ErrorBox error={project.error} /></div>
  if (!p || !ctx) return <div className="empty">{t('Not found')}</div>
  if (p.role !== 'admin')
    return (
      <div className="page">
        <Empty icon={<Shield size={34} />} title={t('Only project admins can change these settings')}>
          {p.archivedAt
            ? t('{name} is archived. Only workspace admins can change or restore archived projects.', { name: p.name })
            : p.leadName
              ? t('Ask {lead} (the project lead) or a workspace admin if something here needs to change.', { lead: p.leadName })
              : t('Ask a project admin or a workspace admin if something here needs to change.')}
        </Empty>
      </div>
    )
  const active = SECTIONS.find((s) => s.id === tab) ?? SECTIONS[0]
  return (
    <SettingsCtx.Provider value={ctx}>
      <div className="page ps-page">
        <div className="page-head">
          <h1>
            <ProjectIcon icon={p.icon} color={p.color} size={28} name={p.name} />
            <span className="ps-title">
              <span>{t('Project settings')}</span>
              <span className="ps-title-sub">{p.name} · {p.key}</span>
            </span>
          </h1>
          {p.archivedAt && <span className="badge badge-warning"><Archive size={12} /> {t('Archived')}</span>}
        </div>
        <div className="ps-layout">
          <nav className="ps-nav" aria-label={t('Project settings')}>
            {SECTIONS.map((s) => (
              <NavLink key={s.id} to={`/projects/${p.key}/settings/${s.id}`} className={() => `ps-nav-item ${s.id === active.id ? 'active' : ''}`}>
                <s.icon size={16} aria-hidden />
                <span>{s.label()}</span>
              </NavLink>
            ))}
          </nav>
          <div className="ps-content">
            {active.id === 'details' && <DetailsSection />}
            {active.id === 'members' && <MembersSection />}
            {active.id === 'workflow' && <WorkflowSection />}
            {active.id === 'types' && <TypesSection />}
            {active.id === 'fields' && <FieldsSection />}
            {active.id === 'boards' && <BoardsSection boardId={sub} />}
            {active.id === 'templates' && <TemplatesSection project={p} />}
            {active.id === 'automation' && <AutomationSection />}
            {active.id === 'sla' && <ServiceLevelsSection key={JSON.stringify(p.settings.sla ?? null)} project={p} onSaved={refresh} />}
            {active.id === 'email' && <MailInSettings project={p} onSaved={refresh} />}
            {active.id === 'service-desk' && <PortalSettings project={p} onSaved={refresh} />}
            {active.id === 'integrations' && <IntegrationsSection />}
            {active.id === 'import' && <ImportSection project={p} />}
            {active.id === 'integrations' && <GitSettings project={p} />}
            {active.id === 'trash' && <TrashSection />}
          </div>
        </div>
      </div>
    </SettingsCtx.Provider>
  )
}

// ---- small building blocks ----

function Section({ title, description, actions, children }: { title: ReactNode; description?: ReactNode; actions?: ReactNode; children: ReactNode }) {
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

/** A list whose rows can be dragged by their grip (or moved with the arrow keys on the grip). */
function Sortable<T extends { id: string }>({ items, onReorder, children, className }: { items: T[]; onReorder: (ids: string[]) => void; children: (item: T, grip: ReactNode, index: number) => ReactNode; className?: string }) {
  const [armed, setArmed] = useState<string | null>(null)
  const [drag, setDrag] = useState<string | null>(null)
  const [over, setOver] = useState<{ id: string; after: boolean } | null>(null)
  useEffect(() => {
    if (!armed || drag) return
    const up = () => setArmed(null)
    window.addEventListener('pointerup', up)
    return () => window.removeEventListener('pointerup', up)
  }, [armed, drag])
  const move = (id: string, target: string, after: boolean) => {
    const before = items.map((i) => i.id)
    const ids = before.filter((x) => x !== id)
    let idx = ids.indexOf(target)
    if (idx < 0) return
    if (after) idx++
    ids.splice(idx, 0, id)
    if (ids.join() !== before.join()) onReorder(ids)
  }
  const reset = () => {
    setDrag(null)
    setOver(null)
    setArmed(null)
  }
  return (
    <div className={`ps-sortable ${className ?? ''}`}>
      {items.map((item, i) => {
        const grip = (
          <button
            type="button"
            className="ps-grip"
            aria-label={t('Reorder (arrow keys move it)')}
            title={t('Drag to reorder')}
            onPointerDown={() => setArmed(item.id)}
            onKeyDown={(e) => {
              if (e.key === 'ArrowUp' && i > 0) {
                e.preventDefault()
                move(item.id, items[i - 1].id, false)
              } else if (e.key === 'ArrowDown' && i < items.length - 1) {
                e.preventDefault()
                move(item.id, items[i + 1].id, true)
              }
            }}
          >
            <GripVertical size={15} />
          </button>
        )
        return (
          <div
            key={item.id}
            className={`ps-sort-item ${drag === item.id ? 'dragging' : ''} ${over?.id === item.id && drag !== item.id ? (over.after ? 'drop-after' : 'drop-before') : ''}`}
            draggable={armed === item.id}
            onDragStart={(e) => {
              setDrag(item.id)
              e.dataTransfer.effectAllowed = 'move'
              e.dataTransfer.setData('text/plain', item.id)
            }}
            onDragEnd={reset}
            onDragOver={(e) => {
              if (!drag) return
              e.preventDefault()
              const r = e.currentTarget.getBoundingClientRect()
              const after = e.clientY > r.top + r.height / 2
              if (over?.id !== item.id || over.after !== after) setOver({ id: item.id, after })
            }}
            onDrop={(e) => {
              e.preventDefault()
              if (drag && over && drag !== over.id) move(drag, over.id, over.after)
              reset()
            }}
          >
            {children(item, grip, i)}
          </div>
        )
      })}
    </div>
  )
}

function ColorPick({ value, onChange, label }: { value: string | null | undefined; onChange: (c: string) => void; label?: string }) {
  return (
    <PopButton className="ps-color-btn" title={label ?? t('Colour')} width={204} label={<span className="ps-color" style={{ background: value ?? 'transparent' }} />}>
      {(close) => <ColorPop value={value} onPick={(c) => { close(); onChange(c) }} />}
    </PopButton>
  )
}

function ColorPop({ value, onPick }: { value: string | null | undefined; onPick: (c: string) => void }) {
  const [custom, setCustom] = useState(value ?? '#8b8d98')
  return (
    <div className="ps-color-pop">
      <div className="swatches">
        {COLORS.map((c) => (
          <button key={c} type="button" className={`swatch ${c === value ? 'on' : ''}`} style={{ background: c }} aria-label={c} title={c} onClick={() => onPick(c)} />
        ))}
      </div>
      <div className="row small">
        <input type="color" className="ps-color-input" value={custom} onChange={(e) => setCustom(e.target.value)} aria-label={t('Custom colour')} />
        <span className="faint">{custom}</span>
        <span className="spacer" />
        <button type="button" className="btn btn-sm" onClick={() => onPick(custom)}>{t('Use')}</button>
      </div>
    </div>
  )
}

function CategoryPick({ value, onChange }: { value: Category; onChange: (c: Category) => void }) {
  return (
    <PopButton className="ps-cat-btn" title={t('Category')} width={260} label={<><Lozenge name={catLabel(value)} category={value} /><ChevronDown size={13} /></>}>
      {(close) => (
        <Choices
          items={CATEGORIES}
          selected={(c) => c === value}
          filterText={catLabel}
          render={(c) => (
            <span className="col ps-gap-2">
              <Lozenge name={catLabel(c)} category={c} />
              <span className="mi-desc">{catHint(c)}</span>
            </span>
          )}
          onPick={(c) => {
            close()
            onChange(c)
          }}
        />
      )}
    </PopButton>
  )
}

/** A text field that saves when it loses focus (or on Enter). */
function InlineText({ value, onSave, placeholder, label, className, required = true }: { value: string; onSave: (v: string) => void; placeholder?: string; label: string; className?: string; required?: boolean }) {
  const [text, setText] = useState(value)
  useEffect(() => setText(value), [value])
  const commit = () => {
    const v = text.trim()
    if (required && !v) return setText(value)
    if (v !== value) onSave(v)
  }
  return (
    <input
      className={`input input-sm ${className ?? ''}`}
      value={text}
      aria-label={label}
      placeholder={placeholder}
      onChange={(e) => setText(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === 'Enter') (e.target as HTMLInputElement).blur()
        if (e.key === 'Escape') {
          setText(value)
          ;(e.target as HTMLInputElement).blur()
        }
      }}
    />
  )
}

/** A TQL input that checks the query as you type. */
function TqlInput({ value, onChange, placeholder, id, onValidity, small }: { value: string; onChange: (v: string) => void; placeholder?: string; id?: string; onValidity?: (ok: boolean) => void; small?: boolean }) {
  const [res, setRes] = useState<{ q: string; ok: boolean; message?: string } | null>(null)
  const cb = useRef(onValidity)
  cb.current = onValidity
  useEffect(() => {
    if (!value.trim()) {
      setRes(null)
      cb.current?.(true)
      return
    }
    let live = true
    const h = setTimeout(async () => {
      try {
        const r = await api.post<{ ok: boolean; message?: string }>('/query/validate', { q: value })
        if (!live) return
        setRes({ q: value, ok: r.ok, message: r.message })
        cb.current?.(r.ok)
      } catch {
        /* offline: the server checks again on save */
      }
    }, 350)
    return () => {
      live = false
      clearTimeout(h)
    }
  }, [value])
  const invalid = !!res && res.q === value && !res.ok
  return (
    <div className="ps-tql">
      <input
        id={id}
        className={`input ${small ? 'input-sm' : ''} query-input ${invalid ? 'invalid' : ''}`}
        value={value}
        spellCheck={false}
        placeholder={placeholder}
        aria-invalid={invalid}
        onChange={(e) => onChange(e.target.value)}
      />
      {invalid && <div className="query-error">{res?.message || t('This query has an error')}</div>}
    </div>
  )
}

/** Delete something whose issues must go somewhere else. */
function DeleteMoveDialog({ title, body, options, onConfirm, onClose }: { title: string; body: ReactNode; options: { id: string; label: string }[]; onConfirm: (moveTo: string) => Promise<void>; onClose: () => void }) {
  const [to, setTo] = useState(options[0]?.id ?? '')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  return (
    <Modal
      title={title}
      onClose={onClose}
      width={460}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button
            className="btn btn-danger-solid"
            disabled={busy || !to}
            onClick={async () => {
              setBusy(true)
              setErr('')
              try {
                await onConfirm(to)
                onClose()
              } catch (e) {
                setErr(errText(e))
              } finally {
                setBusy(false)
              }
            }}
          >
            {t('Delete')}
          </button>
        </>
      }
    >
      <p className="ps-mt-0">{body}</p>
      <div className="field">
        <label htmlFor="ps-move-to">{t('Move its issues to')}</label>
        <select id="ps-move-to" className="select" value={to} onChange={(e) => setTo(e.target.value)}>
          {options.map((o) => <option key={o.id} value={o.id}>{o.label}</option>)}
        </select>
      </div>
      {err && <p className="error-text">{err}</p>}
    </Modal>
  )
}

function SaveBar({ dirty, busy, onSave, onReset, disabled, error }: { dirty: boolean; busy?: boolean; onSave: () => void; onReset: () => void; disabled?: boolean; error?: string }) {
  if (!dirty && !error) return null
  return (
    <div className="ps-savebar" role="region" aria-label={t('Unsaved changes')}>
      <span className="ps-savebar-text">{error ? <span className="error-text">{error}</span> : t('You have unsaved changes')}</span>
      <span className="spacer" />
      <button className="btn" onClick={onReset} disabled={busy}>{t('Discard')}</button>
      <button className="btn btn-primary" onClick={onSave} disabled={busy || disabled || !dirty}>{t('Save changes')}</button>
    </div>
  )
}

/** Follows `base` (server data) into a form's state while the form has no edits of its own. */
function useFollow<T>(base: T, setState: (fn: (cur: T) => T) => void, norm: (v: T) => string = (v) => JSON.stringify(v)) {
  const prev = useRef(base)
  useEffect(() => {
    if (prev.current === base) return
    const old = norm(prev.current)
    prev.current = base
    setState((cur) => (norm(cur) === old ? base : cur))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [base])
}

// ---- details ----

function DetailsSection() {
  const { project, refresh } = useSettings()
  const me = useMe().data
  const people = usePeople().data ?? []
  const nav = useNavigate()
  const toast = useToast()
  const qc = useQueryClient()
  const base = useMemo(
    () => ({
      name: project.name,
      key: project.key,
      description: project.description ?? '',
      icon: project.icon ?? 'rocket',
      color: project.color ?? PROJECT_COLORS[0],
      leadId: project.leadId,
      defaultRole: project.defaultRole,
      estimation: project.settings.estimation ?? 'points',
      defaultAssignee: (project.settings.defaultAssignee as string | undefined) ?? null,
    }),
    [project],
  )
  const [f, setF] = useState(base)
  useFollow(base, setF)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const [confirmKey, setConfirmKey] = useState(false)
  const [archiving, setArchiving] = useState(false)
  const [deleting, setDeleting] = useState(false)
  const set = <K extends keyof typeof f>(k: K, v: (typeof f)[K]) => setF((x) => ({ ...x, [k]: v }))
  const dirty = JSON.stringify(f) !== JSON.stringify(base)
  const keyValid = /^[A-Z][A-Z0-9]{1,9}$/.test(f.key)
  const person = (id: string | null): Named | null => (id ? { id, name: people.find((x) => x.id === id)?.name ?? (id === project.leadId ? project.leadName ?? '…' : '…') } : null)
  const wsAdmin = me?.user?.role === 'owner' || me?.user?.role === 'admin'

  const save = async () => {
    const body: Record<string, unknown> = {}
    const settings: Record<string, unknown> = {}
    if (f.name.trim() !== base.name) body.name = f.name.trim()
    if (f.key !== base.key) body.key = f.key
    if (f.description.trim() !== base.description) body.description = f.description.trim()
    if (f.icon !== base.icon) body.icon = f.icon
    if (f.color !== base.color) body.color = f.color
    if (f.leadId !== base.leadId) body.leadId = f.leadId
    if (f.defaultRole !== base.defaultRole) body.defaultRole = f.defaultRole
    if (f.estimation !== base.estimation) settings.estimation = f.estimation
    if (f.defaultAssignee !== base.defaultAssignee) settings.defaultAssignee = f.defaultAssignee
    if (Object.keys(settings).length) body.settings = settings
    setBusy(true)
    setErr('')
    try {
      const r = await api.patch<{ key: string }>(`/projects/${project.key}`, body)
      setF((x) => ({ ...x, name: x.name.trim(), description: x.description.trim() }))
      refresh()
      if (r?.key && r.key !== project.key) {
        qc.invalidateQueries({ queryKey: ['project', r.key] })
        toast(t('The project key is now {key}', { key: r.key }))
        nav(`/projects/${r.key}/settings/details`, { replace: true })
      } else toast(t('Settings saved'))
    } catch (e) {
      setErr(errText(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <Section title={t('Details')} description={t('Name, key and look of the project, and who leads it.')}>
        <div className="ps-form">
          <div className="ps-grid-2">
            <div className="field">
              <label htmlFor="ps-name">{t('Name')}</label>
              <input id="ps-name" className="input" value={f.name} onChange={(e) => set('name', e.target.value)} />
            </div>
            <div className="field">
              <label htmlFor="ps-key">{t('Key')}</label>
              <input
                id="ps-key"
                className="input ps-mono"
                value={f.key}
                aria-invalid={!keyValid}
                onChange={(e) => set('key', e.target.value.toUpperCase().replace(/[^A-Z0-9]/g, '').slice(0, 10))}
              />
              <span className="field-hint">
                {keyValid
                  ? f.key !== base.key
                    ? t('Issues become {new}-1, {new}-2, …; old keys like {old}-1 keep working in links and searches.', { new: f.key, old: base.key })
                    : t('Issue keys look like {key}-1. Renaming keeps old issue keys and links working.', { key: f.key })
                  : t('2–10 letters or digits, starting with a letter')}
              </span>
            </div>
          </div>
          <div className="field">
            <label htmlFor="ps-desc">{t('Description')}</label>
            <textarea id="ps-desc" className="textarea" rows={2} value={f.description} onChange={(e) => set('description', e.target.value)} />
          </div>
          <div className="ps-grid-2">
            <div className="field">
              <span className="field-label">{t('Colour')}</span>
              <div className="swatches">
                {PROJECT_COLORS.map((c) => (
                  <button key={c} type="button" className={`swatch ${c === f.color ? 'on' : ''}`} style={{ background: c }} aria-label={c} aria-pressed={c === f.color} onClick={() => set('color', c)} />
                ))}
              </div>
            </div>
            <div className="field">
              <span className="field-label">{t('Icon')}</span>
              <div className="icon-grid">
                {Object.keys(PROJECT_ICONS).map((i) => (
                  <button key={i} type="button" className={i === f.icon ? 'on' : ''} onClick={() => set('icon', i)} aria-label={i} aria-pressed={i === f.icon} title={i}>
                    <ProjectIcon icon={i} color={f.color} size={24} />
                  </button>
                ))}
              </div>
            </div>
          </div>
          <div className="ps-grid-2">
            <div className="field">
              <span className="field-label">{t('Project lead')}</span>
              <PersonPicker className="btn ps-person-btn" label={t('Project lead')} value={person(f.leadId)} onChange={(id) => set('leadId', id)} />
            </div>
            <div className="field">
              <span className="field-label">{t('Default assignee')}</span>
              <PersonPicker className="btn ps-person-btn" label={t('Default assignee')} value={person(f.defaultAssignee)} onChange={(id) => set('defaultAssignee', id)} />
              <span className="field-hint">{t('New issues are assigned to this person unless someone is picked.')}</span>
            </div>
          </div>
          <div className="ps-grid-2">
            <div className="field">
              <span className="field-label">{t('Estimation')}</span>
              <div className="seg" role="radiogroup" aria-label={t('Estimation')}>
                <button type="button" role="radio" aria-checked={f.estimation === 'points'} className={f.estimation === 'points' ? 'active' : ''} onClick={() => set('estimation', 'points')}>{t('Story points')}</button>
                <button type="button" role="radio" aria-checked={f.estimation === 'hours'} className={f.estimation === 'hours' ? 'active' : ''} onClick={() => set('estimation', 'hours')}>{t('Hours')}</button>
              </div>
              <span className="field-hint">{t('What backlogs, sprints and reports add up.')}</span>
            </div>
            <div className="field">
              <label htmlFor="ps-role">{t('Default role for workspace members')}</label>
              <select id="ps-role" className="select" value={f.defaultRole} onChange={(e) => set('defaultRole', e.target.value as Role)}>
                {(['none', 'viewer', 'member', 'admin'] as Role[]).map((r) => <option key={r} value={r}>{roleLabel(r)}</option>)}
              </select>
              <span className="field-hint">
                {roleHint(f.defaultRole)}. {t('Guests only get what Members grants them.')}
              </span>
            </div>
          </div>
        </div>
        <SaveBar
          dirty={dirty}
          busy={busy}
          error={err}
          disabled={!keyValid || !f.name.trim()}
          onReset={() => {
            setF(base)
            setErr('')
          }}
          onSave={() => (f.key !== base.key ? setConfirmKey(true) : save())}
        />
      </Section>

      <Section title={t('Archive or delete')}>
        <div className="ps-danger">
          <div className="ps-danger-row">
            <div>
              <div className="ps-strong">{project.archivedAt ? t('Restore project') : t('Archive project')}</div>
              <div className="faint small">
                {project.archivedAt
                  ? t('Archived {when}. Restoring makes it editable again and brings it back to the project list.', { when: ago(project.archivedAt) })
                  : t('Archived projects are read-only and hidden from the project list; workspace admins can restore them.')}
              </div>
            </div>
            {project.archivedAt ? (
              <button className="btn" onClick={() => setArchiving(true)}><ArchiveArrowUp size={15} /> {t('Restore')}</button>
            ) : (
              <button className="btn" onClick={() => setArchiving(true)}><Archive size={15} /> {t('Archive')}</button>
            )}
          </div>
          <div className="ps-danger-row">
            <div>
              <div className="ps-strong">{t('Delete project')}</div>
              <div className="faint small">
                {wsAdmin ? t('Deletes the project with all its issues, comments and attachments. This can’t be undone.') : t('Only workspace admins can delete projects.')}
              </div>
            </div>
            <button className="btn btn-danger" disabled={!wsAdmin} onClick={() => setDeleting(true)}><Trash size={15} /> {t('Delete')}</button>
          </div>
        </div>
      </Section>

      {confirmKey && (
        <Confirm
          title={t('Change the key to {key}?', { key: f.key })}
          body={<p>{t('Issue keys change from {old}-12 to {new}-12. Old keys keep working in links, searches and the API, and {old} can’t be used by another project.', { old: base.key, new: f.key })}</p>}
          confirmLabel={t('Change key and save')}
          onConfirm={save}
          onClose={() => setConfirmKey(false)}
        />
      )}
      {archiving && (
        <Confirm
          title={project.archivedAt ? t('Restore {name}?', { name: project.name }) : t('Archive {name}?', { name: project.name })}
          body={<p>{project.archivedAt ? t('The project becomes editable again for its members.') : t('Everyone except workspace admins can only read it afterwards. You can restore it later.')}</p>}
          confirmLabel={project.archivedAt ? t('Restore') : t('Archive')}
          onConfirm={async () => {
            const archived = !project.archivedAt
            try {
              await api.post(`/projects/${project.key}/archive`, { archived })
              toast(archived ? t('{name} archived', { name: project.name }) : t('{name} restored', { name: project.name }))
              if (archived && !wsAdmin) nav('/projects')
            } catch (e) {
              toast(errText(e))
            } finally {
              refresh()
            }
          }}
          onClose={() => setArchiving(false)}
        />
      )}
      {deleting && <DeleteProjectDialog project={project} onClose={() => setDeleting(false)} />}
    </>
  )
}

function DeleteProjectDialog({ project, onClose }: { project: Project; onClose: () => void }) {
  const [typed, setTyped] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const nav = useNavigate()
  const qc = useQueryClient()
  const toast = useToast()
  const ok = typed.trim().toUpperCase() === project.key
  return (
    <Modal
      title={t('Delete {name}?', { name: project.name })}
      onClose={onClose}
      width={460}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button
            className="btn btn-danger-solid"
            disabled={!ok || busy}
            onClick={async () => {
              setBusy(true)
              setErr('')
              try {
                await api.del(`/projects/${project.key}`, { confirm: typed.trim() })
                qc.removeQueries({ queryKey: ['project', project.key] })
                qc.invalidateQueries({ queryKey: ['projects'] })
                toast(t('{name} deleted', { name: project.name }))
                nav('/projects', { replace: true })
              } catch (e) {
                setErr(errText(e))
                setBusy(false)
              }
            }}
          >
            {t('Delete project')}
          </button>
        </>
      }
    >
      <div className="ps-warn">
        <TriangleExclamationPoint size={18} aria-hidden />
        <span>{t('All issues, comments, attachments, boards and settings of this project are deleted for good.')}</span>
      </div>
      <div className="field">
        <label htmlFor="ps-confirm">{t('Type {key} to confirm', { key: project.key })}</label>
        <input id="ps-confirm" className="input ps-mono" autoFocus value={typed} onChange={(e) => setTyped(e.target.value)} autoComplete="off" />
      </div>
      {err && <p className="error-text">{err}</p>}
    </Modal>
  )
}

// ---- members ----

function MembersSection() {
  const { project, refresh } = useSettings()
  const qc = useQueryClient()
  const toast = useToast()
  const qk = ['project-members', project.key]
  const members = useQuery({ queryKey: qk, queryFn: () => api.get<MembersData>(`/projects/${project.key}/members`) })
  const people = usePeople()
  const teams = useQuery({ queryKey: ['teams'], queryFn: () => api.get<Team[]>('/teams'), staleTime: 60_000 })
  const [showAll, setShowAll] = useState(false)
  const grants = members.data?.grants ?? []

  const save = async (next: Grant[]) => {
    if (!keepsAdmin(next)) {
      toast(t('You’d lose admin rights on this project. Keep yourself (or a team of yours) as admin.'))
      return
    }
    qc.setQueryData<MembersData>(qk, (old) => (old ? { ...old, grants: next } : old))
    try {
      await api.put(`/projects/${project.key}/members`, next.map((g) => ({ subjectType: g.subjectType, subjectId: g.subjectId, role: g.role })))
    } catch (e) {
      toast(errText(e))
    }
    qc.invalidateQueries({ queryKey: qk })
    refresh()
  }
  const me = useMe().data?.user
  /** Whether the current person stays a project admin with these grants (workspace admins always do). */
  const keepsAdmin = (next: Grant[]) => {
    if (!me || me.role === 'owner' || me.role === 'admin') return true
    if ((members.data?.defaultRole ?? project.defaultRole) === 'admin' && me.role !== 'guest') return true
    if (!teams.data) return true
    return next.some(
      (g) => g.role === 'admin' && ((g.subjectType === 'user' && g.subjectId === me.id) || (g.subjectType === 'team' && teams.data!.some((x) => x.id === g.subjectId && x.memberIds.includes(me.id)))),
    )
  }
  const has = (type: string, id: string) => grants.some((g) => g.subjectType === type && g.subjectId === id)
  const freePeople = (people.data ?? []).filter((p) => !has('user', p.id))
  const freeTeams = (teams.data ?? []).filter((x) => !has('team', x.id))

  if (members.isLoading) return <Loading />
  if (members.error) return <ErrorBox error={members.error} />
  return (
    <>
      <Section
        title={t('Members')}
        description={t('People and teams with their own role in this project. Workspace owners and admins are admins in every project.')}
        actions={
          <>
            <PopButton className="btn" width={280} label={<><Plus size={15} /> {t('Add people')}</>} title={t('Add people')}>
              {(close) => (
                <Choices
                  items={freePeople}
                  placeholder={t('Search people…')}
                  empty={t('Everyone is already listed')}
                  filterText={(p) => `${p.name} ${p.email ?? ''}`}
                  render={(p) => (
                    <>
                      <Avatar name={p.name} id={p.id} size={22} />
                      <span className="col ps-gap-0">
                        <span>{p.name}</span>
                        {p.email && <span className="mi-desc">{p.email}</span>}
                      </span>
                    </>
                  )}
                  onPick={(p) => {
                    close()
                    save([...grants, { subjectType: 'user', subjectId: p.id, role: 'member', name: p.name }])
                  }}
                />
              )}
            </PopButton>
            <PopButton className="btn" width={280} label={<><Users size={15} /> {t('Add team')}</>} title={t('Add team')}>
              {(close) => (
                <Choices
                  items={freeTeams}
                  placeholder={t('Search teams…')}
                  empty={teams.data?.length ? t('All teams are already listed') : t('No teams yet (workspace admins create them under Admin)')}
                  filterText={(x) => x.name}
                  render={(x) => (
                    <>
                      <span className="ps-team-icon" style={x.color ? { background: x.color } : undefined}><Users size={13} /></span>
                      <span className="col ps-gap-0">
                        <span>{x.name}</span>
                        <span className="mi-desc">{t('{count} people', { count: x.memberIds.length })}</span>
                      </span>
                    </>
                  )}
                  onPick={(x) => {
                    close()
                    save([...grants, { subjectType: 'team', subjectId: x.id, role: 'member', name: x.name }])
                  }}
                />
              )}
            </PopButton>
          </>
        }
      >
        <div className="ps-note">
          <Info size={16} aria-hidden />
          <span>
            {t('Everyone else in the workspace (except guests) gets the default role: {role}.', { role: roleLabel(members.data?.defaultRole ?? project.defaultRole) })}{' '}
            <Link to={`/projects/${project.key}/settings/details`}>{t('Change the default role')}</Link>
          </span>
        </div>
        {grants.length === 0 ? (
          <Empty icon={<Users size={30} />} title={t('No one added yet')}>{t('Add people or teams to give them a role here.')}</Empty>
        ) : (
          <div className="ps-table-wrap">
            <table className="list ps-table">
              <thead>
                <tr>
                  <th>{t('Name')}</th>
                  <th>{t('Role')}</th>
                  <th aria-label={t('Actions')} />
                </tr>
              </thead>
              <tbody>
                {grants.map((g) => {
                  const team = g.subjectType === 'team' ? teams.data?.find((x) => x.id === g.subjectId) : undefined
                  return (
                    <tr key={`${g.subjectType}:${g.subjectId}`}>
                      <td>
                        <span className="row">
                          {g.subjectType === 'team' ? (
                            <span className="ps-team-icon" style={team?.color ? { background: team.color } : undefined}><Users size={13} /></span>
                          ) : (
                            <Avatar name={g.name ?? '?'} id={g.subjectId} size={24} />
                          )}
                          <span className="col ps-gap-0">
                            <span>{g.name ?? t('Unknown')}</span>
                            {g.subjectType === 'team' && <span className="faint small">{t('Team · {count} people', { count: team?.memberIds.length ?? 0 })}</span>}
                          </span>
                        </span>
                      </td>
                      <td>
                        <select
                          className="select input-sm ps-role-select"
                          aria-label={t('Role of {name}', { name: g.name ?? '' })}
                          value={g.role}
                          onChange={(e) => save(grants.map((x) => (x === g ? { ...x, role: e.target.value as Role } : x)))}
                        >
                          {(['viewer', 'member', 'admin'] as Role[]).map((r) => <option key={r} value={r}>{roleLabel(r)}</option>)}
                        </select>
                      </td>
                      <td className="ps-td-actions">
                        <button className="icon-btn" title={t('Remove')} aria-label={t('Remove {name}', { name: g.name ?? '' })} onClick={() => save(grants.filter((x) => x !== g))}>
                          <X size={15} />
                        </button>
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
        )}
      </Section>

      <Section title={t('Roles')}>
        <dl className="ps-roles">
          {(['viewer', 'member', 'admin'] as Role[]).map((r) => (
            <div key={r}>
              <dt><span className={`badge ${r === 'admin' ? 'badge-accent' : ''}`}>{roleLabel(r)}</span></dt>
              <dd>{roleHint(r)}</dd>
            </div>
          ))}
        </dl>
        <p className="faint small">{t('A person gets the highest of: the default role, their own grant and the grants of their teams. Guests only get grants.')}</p>
      </Section>

      <Section title={t('Who has access')} description={t('{count} people can see this project.', { count: members.data?.people.length ?? 0 })}>
        <button className="btn btn-ghost btn-sm" onClick={() => setShowAll((s) => !s)} aria-expanded={showAll}>
          {showAll ? <ChevronDown size={14} /> : <ChevronRight size={14} />} {showAll ? t('Hide list') : t('Show everyone')}
        </button>
        {showAll && (
          <div className="ps-people">
            {members.data?.people.map((p) => (
              <div key={p.id} className="ps-person">
                <Avatar name={p.name} id={p.id} size={22} />
                <span className="ps-ellipsis">{p.name}</span>
                <span className="spacer" />
                <span className={`badge ${p.role === 'admin' ? 'badge-accent' : ''}`}>{roleLabel(p.role)}</span>
              </div>
            ))}
          </div>
        )}
      </Section>
    </>
  )
}

// ---- workflow ----

function WorkflowSection() {
  const { project, run, patch } = useSettings()
  const [deleting, setDeleting] = useState<Status | null>(null)
  const [name, setName] = useState('')
  const [cat, setCat] = useState<Category>('todo')
  const [color, setColor] = useState('#5b8def')
  const statuses = project.statuses

  const update = (s: Status, body: Partial<Status>) => {
    patch((p) => ({ ...p, statuses: p.statuses.map((x) => (x.id === s.id ? { ...x, ...body } : x)) }))
    run(() => api.patch(`/statuses/${s.id}`, body))
  }
  const reorder = (ids: string[]) => {
    patch((p) => ({ ...p, statuses: ids.map((id) => p.statuses.find((x) => x.id === id)!).filter(Boolean) }))
    run(() => api.put(`/projects/${project.key}/order/statuses`, { ids }))
  }
  const add = async () => {
    if (!name.trim()) return
    const ok = await run(() => api.post(`/projects/${project.key}/statuses`, { name: name.trim(), category: cat, color }), t('Status {name} added', { name: name.trim() }))
    if (ok) setName('')
  }

  return (
    <>
      <Section
        title={t('Statuses')}
        description={t('The steps an issue goes through, in board order. The category decides what counts as started and finished in reports.')}
      >
        <Sortable items={statuses} onReorder={reorder} className="ps-list">
          {(s, grip) => (
            <div className="ps-row">
              {grip}
              <ColorPick value={s.color} onChange={(c) => update(s, { color: c })} label={t('Colour of {name}', { name: s.name })} />
              <InlineText className="ps-grow" value={s.name} label={t('Status name')} onSave={(v) => update(s, { name: v })} />
              <CategoryPick value={s.category} onChange={(c) => update(s, { category: c })} />
              <button
                className="icon-btn"
                title={t('Delete status')}
                aria-label={t('Delete {name}', { name: s.name })}
                disabled={statuses.length <= 1}
                onClick={() => setDeleting(s)}
              >
                <Trash size={15} />
              </button>
            </div>
          )}
        </Sortable>
        <form
          className="ps-row ps-add-row"
          onSubmit={(e) => {
            e.preventDefault()
            add()
          }}
        >
          <ColorPick value={color} onChange={setColor} />
          <input className="input input-sm ps-grow" placeholder={t('New status name, e.g. In QA')} aria-label={t('New status name')} value={name} onChange={(e) => setName(e.target.value)} />
          <CategoryPick value={cat} onChange={setCat} />
          <button className="btn btn-sm btn-primary" type="submit" disabled={!name.trim()}><Plus size={14} /> {t('Add status')}</button>
        </form>
      </Section>

      <TransitionsEditor key={`${statuses.map((s) => s.id).join()}|${project.settings.workflow}|${project.transitions.map((x) => x.from + x.to).join()}`} />

      {deleting && (
        <DeleteMoveDialog
          title={t('Delete status {name}?', { name: deleting.name })}
          body={t('Issues with this status move to the status you pick. Boards forget the status; empty columns are removed.')}
          options={statuses.filter((s) => s.id !== deleting.id).map((s) => ({ id: s.id, label: `${s.name} (${catLabel(s.category)})` }))}
          onConfirm={async (moveTo) => {
            await api.del(`/statuses/${deleting.id}?moveTo=${encodeURIComponent(moveTo)}`)
            run(async () => {}, t('Status {name} deleted', { name: deleting.name }))
          }}
          onClose={() => setDeleting(null)}
        />
      )}
    </>
  )
}

function TransitionsEditor() {
  const { project, run } = useSettings()
  const statuses = project.statuses
  const all = () => {
    const s = new Set<string>()
    statuses.forEach((a) => statuses.forEach((b) => a.id !== b.id && s.add(`${a.id}>${b.id}`)))
    return s
  }
  const initial = () => (project.transitions.length ? new Set(project.transitions.map((x) => `${x.from}>${x.to}`)) : all())
  const [mode, setMode] = useState<'free' | 'restricted'>(project.settings.workflow ?? 'free')
  const [allowed, setAllowed] = useState<Set<string>>(initial)
  const [busy, setBusy] = useState(false)
  const baseMode = project.settings.workflow ?? 'free'
  const sig = (s: Set<string>) => [...s].sort().join()
  const dirty = mode !== baseMode || (mode === 'restricted' && sig(allowed) !== sig(initial()))
  const toggle = (k: string) =>
    setAllowed((s) => {
      const n = new Set(s)
      if (n.has(k)) n.delete(k)
      else n.add(k)
      return n
    })
  const neighbours = () => {
    const s = new Set<string>()
    statuses.forEach((a, i) => {
      if (statuses[i + 1]) s.add(`${a.id}>${statuses[i + 1].id}`)
      if (statuses[i - 1]) s.add(`${a.id}>${statuses[i - 1].id}`)
    })
    return s
  }
  const name = (id: string) => statuses.find((s) => s.id === id)
  const save = async () => {
    setBusy(true)
    const transitions = [...allowed].map((k) => {
      const [from, to] = k.split('>')
      return { from, to }
    })
    await run(() => api.put(`/projects/${project.key}/transitions`, { workflow: mode, transitions }), t('Workflow saved'))
    setBusy(false)
  }
  return (
    <Section
      title={t('Allowed moves')}
      description={t('Free: an issue can move from any status to any other. Restricted: only the moves ticked below (project admins can always move issues anywhere).')}
      actions={
        <div className="seg" role="radiogroup" aria-label={t('Workflow mode')}>
          <button type="button" role="radio" aria-checked={mode === 'free'} className={mode === 'free' ? 'active' : ''} onClick={() => setMode('free')}><LockOpen size={14} /> {t('Free')}</button>
          <button type="button" role="radio" aria-checked={mode === 'restricted'} className={mode === 'restricted' ? 'active' : ''} onClick={() => setMode('restricted')}><Lock size={14} /> {t('Restricted')}</button>
        </div>
      }
    >
      {mode === 'free' ? (
        <div className="ps-note"><Info size={16} aria-hidden /><span>{t('Any status can move to any other status.')}</span></div>
      ) : (
        <>
          <div className="toolbar">
            <button className="btn btn-sm" onClick={() => setAllowed(all())}>{t('Allow every move')}</button>
            <button className="btn btn-sm" onClick={() => setAllowed(neighbours())}>{t('Only next and previous step')}</button>
            <button className="btn btn-sm" onClick={() => setAllowed(new Set())}>{t('Clear')}</button>
          </div>
          <div className="ps-matrix-wrap">
            <table className="ps-matrix">
              <thead>
                <tr>
                  <th className="ps-matrix-corner"><span className="faint small">{t('From ↓ / to →')}</span></th>
                  {statuses.map((s) => (
                    <th key={s.id} scope="col"><Lozenge name={s.name} category={s.category} /></th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {statuses.map((from) => (
                  <tr key={from.id}>
                    <th scope="row"><Lozenge name={from.name} category={from.category} /></th>
                    {statuses.map((to) => {
                      const k = `${from.id}>${to.id}`
                      return (
                        <td key={to.id}>
                          {from.id === to.id ? (
                            <span className="faint" aria-hidden>—</span>
                          ) : (
                            <input type="checkbox" checked={allowed.has(k)} onChange={() => toggle(k)} aria-label={t('Allow {from} → {to}', { from: from.name, to: to.name })} />
                          )}
                        </td>
                      )
                    })}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="ps-flow" aria-label={t('Preview')}>
            <div className="ps-flow-title">{t('Preview')}</div>
            {statuses.map((s) => {
              const targets = statuses.filter((x) => allowed.has(`${s.id}>${x.id}`))
              return (
                <div key={s.id} className="ps-flow-row">
                  <span className="ps-flow-from"><Lozenge name={s.name} category={s.category} /></span>
                  <ArrowRight size={14} className="faint" aria-hidden />
                  <span className="ps-flow-to">
                    {targets.length ? targets.map((x) => <Lozenge key={x.id} name={name(x.id)?.name ?? ''} category={x.category} />) : <span className="ps-flow-dead">{t('No way out (a dead end)')}</span>}
                  </span>
                </div>
              )
            })}
          </div>
        </>
      )}
      <SaveBar dirty={dirty} busy={busy} onSave={save} onReset={() => { setMode(baseMode); setAllowed(initial()) }} />
    </Section>
  )
}

// ---- issue types ----

function TypesSection() {
  const { project, run, patch } = useSettings()
  const [editing, setEditing] = useState<IssueType | 'new' | null>(null)
  const [deleting, setDeleting] = useState<IssueType | null>(null)
  const types = project.types
  const reorder = (ids: string[]) => {
    patch((p) => ({ ...p, types: ids.map((id) => p.types.find((x) => x.id === id)!).filter(Boolean) }))
    run(() => api.put(`/projects/${project.key}/order/types`, { ids }))
  }
  return (
    <Section
      title={t('Issue types')}
      description={t('The kinds of work this project tracks. Epics group standard issues; sub-tasks split them up. The first standard type is the default for new issues.')}
      actions={<button className="btn btn-primary" onClick={() => setEditing('new')}><Plus size={15} /> {t('Add type')}</button>}
    >
      <Sortable items={types} onReorder={reorder} className="ps-list">
        {(x, grip) => (
          <div className="ps-row">
            {grip}
            <TypeIcon icon={x.icon} color={x.color} size={22} />
            <button className="ps-row-main ps-link-btn" onClick={() => setEditing(x)}>
              <span className="ps-strong">{x.name}</span>
              {x.template && <span className="faint small"><span className="sr-only">· </span>{t('has a description template')}</span>}
            </button>
            <span className={`badge ${x.level > 0 ? 'badge-accent' : ''}`}>{levelLabel(x.level)}</span>
            <button className="icon-btn" title={t('Edit')} aria-label={t('Edit {name}', { name: x.name })} onClick={() => setEditing(x)}><Pencil size={15} /></button>
            <button className="icon-btn" title={t('Delete')} aria-label={t('Delete {name}', { name: x.name })} disabled={types.length <= 1} onClick={() => setDeleting(x)}><Trash size={15} /></button>
          </div>
        )}
      </Sortable>
      {editing && <TypeDialog type={editing === 'new' ? null : editing} onClose={() => setEditing(null)} />}
      {deleting && (
        <DeleteMoveDialog
          title={t('Delete type {name}?', { name: deleting.name })}
          body={t('Issues of this type get the type you pick.')}
          options={types
            .filter((x) => x.id !== deleting.id)
            .sort((a, b) => Number(b.level === deleting.level) - Number(a.level === deleting.level))
            .map((x) => ({ id: x.id, label: `${x.name} (${levelLabel(x.level)})` }))}
          onConfirm={async (moveTo) => {
            await api.del(`/types/${deleting.id}?moveTo=${encodeURIComponent(moveTo)}`)
            run(async () => {}, t('Type {name} deleted', { name: deleting.name }))
          }}
          onClose={() => setDeleting(null)}
        />
      )}
    </Section>
  )
}

function TypeDialog({ type, onClose }: { type: IssueType | null; onClose: () => void }) {
  const { project, refresh } = useSettings()
  const toast = useToast()
  const [name, setName] = useState(type?.name ?? '')
  const [icon, setIcon] = useState(type?.icon ?? 'task')
  const [color, setColor] = useState(type?.color ?? '#3b82f6')
  const [level, setLevel] = useState(type?.level ?? 0)
  const initialTemplate = useMemo(() => docToMarkdown(type?.template), [type])
  const [template, setTemplate] = useState(initialTemplate)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const submit = async () => {
    setBusy(true)
    setErr('')
    try {
      if (type) {
        const body: Record<string, unknown> = { name: name.trim(), icon, color, level }
        if (template !== initialTemplate) body.template = template
        await api.patch(`/types/${type.id}`, body)
      } else {
        const r = await api.post<{ id: string }>(`/projects/${project.key}/types`, { name: name.trim(), icon, color, level })
        if (template.trim()) await api.patch(`/types/${r.id}`, { template })
      }
      refresh()
      toast(type ? t('Type saved') : t('Type {name} added', { name: name.trim() }))
      onClose()
    } catch (e) {
      setErr(errText(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal
      title={type ? t('Edit issue type') : t('New issue type')}
      onClose={onClose}
      width={620}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !name.trim()} onClick={submit}>{type ? t('Save') : t('Add type')}</button>
        </>
      }
    >
      <div className="row ps-align-start ps-gap-12">
        <div className="ps-type-preview" aria-hidden><TypeIcon icon={icon} color={color} size={40} /></div>
        <div className="field ps-grow">
          <label htmlFor="ps-type-name">{t('Name')}</label>
          <input id="ps-type-name" className="input" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder={t('e.g. Improvement')} />
        </div>
      </div>
      <div className="field">
        <span className="field-label">{t('Icon')}</span>
        <div className="icon-grid">
          {Object.keys(TYPE_ICONS).map((i) => (
            <button key={i} type="button" className={i === icon ? 'on' : ''} onClick={() => setIcon(i)} aria-label={i} aria-pressed={i === icon} title={i}>
              <TypeIcon icon={i} color={color} size={24} />
            </button>
          ))}
        </div>
      </div>
      <div className="field">
        <span className="field-label">{t('Colour')}</span>
        <div className="swatches">
          {COLORS.map((c) => (
            <button key={c} type="button" className={`swatch ${c === color ? 'on' : ''}`} style={{ background: c }} aria-label={c} aria-pressed={c === color} onClick={() => setColor(c)} />
          ))}
          <input type="color" className="ps-color-input" value={color} onChange={(e) => setColor(e.target.value)} aria-label={t('Custom colour')} title={t('Custom colour')} />
        </div>
      </div>
      <div className="field">
        <span className="field-label">{t('Level')}</span>
        <div className="seg" role="radiogroup" aria-label={t('Level')}>
          {[1, 0, -1].map((l) => (
            <button key={l} type="button" role="radio" aria-checked={level === l} className={level === l ? 'active' : ''} onClick={() => setLevel(l)}>{levelLabel(l)}</button>
          ))}
        </div>
        <span className="field-hint">{levelHint(level)}</span>
      </div>
      <div className="field">
        <label htmlFor="ps-type-template">{t('Description template (Markdown)')}</label>
        <textarea
          id="ps-type-template"
          className="textarea ps-mono"
          rows={7}
          value={template}
          onChange={(e) => setTemplate(e.target.value)}
          placeholder={'**' + t('Steps to reproduce') + '**\n\n1. \n\n**' + t('Expected') + '**\n'}
        />
        <span className="field-hint">{t('New issues of this type start with this description. Leave empty for none.')}</span>
      </div>
      {err && <p className="error-text">{err}</p>}
    </Modal>
  )
}

// ---- custom fields and components ----

const FIELD_KINDS: { id: CustomField['kind']; icon: typeof Type; label: () => string }[] = [
  { id: 'text', icon: Type, label: () => t('Text') },
  { id: 'number', icon: Hash, label: () => t('Number') },
  { id: 'select', icon: CircleDot, label: () => t('Select (one option)') },
  { id: 'multiselect', icon: ListChecks, label: () => t('Multi-select') },
  { id: 'date', icon: Calendar, label: () => t('Date') },
  { id: 'user', icon: User, label: () => t('Person') },
  { id: 'checkbox', icon: SquareCheck, label: () => t('Checkbox') },
  { id: 'url', icon: LinkIcon, label: () => t('Link (URL)') },
]
const kindMeta = (k: CustomField['kind']) => FIELD_KINDS.find((x) => x.id === k) ?? FIELD_KINDS[0]

function FieldsSection() {
  const { project, run, patch } = useSettings()
  const [editing, setEditing] = useState<CustomField | 'new' | null>(null)
  const [deleting, setDeleting] = useState<CustomField | null>(null)
  const reorder = (ids: string[]) => {
    patch((p) => ({ ...p, fields: ids.map((id) => p.fields.find((x) => x.id === id)!).filter(Boolean) }))
    run(() => api.put(`/projects/${project.key}/order/fields`, { ids }))
  }
  return (
    <>
      <Section
        title={t('Custom fields')}
        description={t('Extra fields on every issue of this project. Search them in TQL by name, e.g. Severity = Critical.')}
        actions={<button className="btn btn-primary" onClick={() => setEditing('new')}><Plus size={15} /> {t('Add field')}</button>}
      >
        {project.fields.length === 0 ? (
          <Empty icon={<TextCursorInput size={30} />} title={t('No custom fields')}>{t('Add fields like Severity, Environment or Budget.')}</Empty>
        ) : (
          <Sortable items={project.fields} onReorder={reorder} className="ps-list">
            {(f, grip) => {
              const K = kindMeta(f.kind)
              return (
                <div className="ps-row">
                  {grip}
                  <span className="ps-kind-icon" title={K.label()}><K.icon size={15} aria-hidden /></span>
                  <button className="ps-row-main ps-link-btn" onClick={() => setEditing(f)}>
                    <span className="ps-strong">{f.name}</span>
                    <span className="faint small">{K.label()}{f.description ? ` · ${f.description}` : ''}</span>
                  </button>
                  {(f.kind === 'select' || f.kind === 'multiselect') && (
                    <span className="ps-chips ps-hide-phone">
                      {f.options.slice(0, 4).map((o) => <OptionChip key={o.id} label={o.label} color={o.color} />)}
                      {f.options.length > 4 && <span className="faint small">+{f.options.length - 4}</span>}
                    </span>
                  )}
                  <button className="icon-btn" title={t('Edit')} aria-label={t('Edit {name}', { name: f.name })} onClick={() => setEditing(f)}><Pencil size={15} /></button>
                  <button className="icon-btn" title={t('Delete')} aria-label={t('Delete {name}', { name: f.name })} onClick={() => setDeleting(f)}><Trash size={15} /></button>
                </div>
              )
            }}
          </Sortable>
        )}
      </Section>
      <ComponentsEditor />
      {editing && <FieldDialog field={editing === 'new' ? null : editing} onClose={() => setEditing(null)} />}
      {deleting && (
        <Confirm
          title={t('Delete field {name}?', { name: deleting.name })}
          body={<p>{t('Its values are removed from every issue. This can’t be undone.')}</p>}
          confirmLabel={t('Delete field')}
          danger
          onConfirm={() => run(() => api.del(`/fields/${deleting.id}`), t('Field {name} deleted', { name: deleting.name })).then(() => undefined)}
          onClose={() => setDeleting(null)}
        />
      )}
    </>
  )
}

type DraftOption = FieldOption & { k: string }

function FieldDialog({ field, onClose }: { field: CustomField | null; onClose: () => void }) {
  const { project, refresh } = useSettings()
  const toast = useToast()
  const [name, setName] = useState(field?.name ?? '')
  const [kind, setKind] = useState<CustomField['kind']>(field?.kind ?? 'text')
  const [description, setDescription] = useState(field?.description ?? '')
  const [options, setOptions] = useState<DraftOption[]>(() => (field?.options ?? []).map((o) => ({ ...o, k: o.id })))
  const [newOption, setNewOption] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const withOptions = kind === 'select' || kind === 'multiselect'
  const addOption = () => {
    const label = newOption.trim()
    if (!label) return
    setOptions((o) => [...o, { id: optionId(), k: newId(), label, color: COLORS[o.length % COLORS.length] }])
    setNewOption('')
  }
  const setOpt = (k: string, v: Partial<DraftOption>) => setOptions((o) => o.map((x) => (x.k === k ? { ...x, ...v } : x)))
  const submit = async () => {
    setBusy(true)
    setErr('')
    const opts = withOptions ? options.filter((o) => o.label.trim()).map((o) => ({ id: o.id || optionId(), label: o.label.trim(), color: o.color ?? null })) : []
    try {
      if (field) await api.patch(`/fields/${field.id}`, { name: name.trim(), description: description.trim() || null, ...(withOptions ? { options: opts } : {}) })
      else await api.post(`/projects/${project.key}/fields`, { name: name.trim(), kind, description: description.trim() || null, options: opts })
      refresh()
      toast(field ? t('Field saved') : t('Field {name} added', { name: name.trim() }))
      onClose()
    } catch (e) {
      setErr(errText(e))
    } finally {
      setBusy(false)
    }
  }
  const removed = field ? field.options.filter((o) => !options.some((x) => x.id === o.id)) : []
  return (
    <Modal
      title={field ? t('Edit field') : t('New custom field')}
      onClose={onClose}
      width={560}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !name.trim() || (withOptions && options.filter((o) => o.label.trim()).length === 0)} onClick={submit}>
            {field ? t('Save') : t('Add field')}
          </button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="ps-field-name">{t('Name')}</label>
        <input id="ps-field-name" className="input" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder={t('e.g. Severity')} />
      </div>
      <div className="field">
        <span className="field-label">{t('Kind')}</span>
        {field ? (
          <div className="row">
            {(() => {
              const K = kindMeta(field.kind)
              return <><K.icon size={15} aria-hidden /> {K.label()}</>
            })()}
            <span className="faint small">{t('(can’t be changed after creation)')}</span>
          </div>
        ) : (
          <div className="ps-kind-grid" role="radiogroup" aria-label={t('Kind')}>
            {FIELD_KINDS.map((k) => (
              <button key={k.id} type="button" role="radio" aria-checked={kind === k.id} className={`ps-kind ${kind === k.id ? 'on' : ''}`} onClick={() => setKind(k.id)}>
                <k.icon size={15} aria-hidden /> {k.label()}
              </button>
            ))}
          </div>
        )}
      </div>
      <div className="field">
        <label htmlFor="ps-field-desc">{t('Description (optional)')}</label>
        <input id="ps-field-desc" className="input" value={description} onChange={(e) => setDescription(e.target.value)} placeholder={t('Shown as help next to the field')} />
      </div>
      {withOptions && (
        <div className="field">
          <span className="field-label">{t('Options')}</span>
          <Sortable
            items={options.map((o) => ({ ...o, id: o.k, realId: o.id }))}
            onReorder={(ks) => setOptions((o) => ks.map((k) => o.find((x) => x.k === k)!).filter(Boolean))}
            className="ps-list ps-list-tight"
          >
            {(o, grip) => (
              <div className="ps-row">
                {grip}
                <ColorPick value={o.color} onChange={(c) => setOpt(o.id, { color: c })} label={t('Colour of {name}', { name: o.label })} />
                <input className="input input-sm ps-grow" value={o.label} aria-label={t('Option label')} onChange={(e) => setOpt(o.id, { label: e.target.value })} />
                <button className="icon-btn" title={t('Remove option')} aria-label={t('Remove {name}', { name: o.label })} onClick={() => setOptions((x) => x.filter((y) => y.k !== o.id))}><X size={15} /></button>
              </div>
            )}
          </Sortable>
          <div className="row">
            <input
              className="input input-sm ps-grow"
              placeholder={t('New option')}
              aria-label={t('New option')}
              value={newOption}
              onChange={(e) => setNewOption(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault()
                  addOption()
                }
              }}
            />
            <button className="btn btn-sm" onClick={addOption} disabled={!newOption.trim()}><Plus size={14} /> {t('Add option')}</button>
          </div>
          {removed.length > 0 && (
            <span className="field-hint ps-warn-text">{t('Issues that use {options} lose that value.', { options: removed.map((o) => o.label).join(', ') })}</span>
          )}
        </div>
      )}
      {err && <p className="error-text">{err}</p>}
    </Modal>
  )
}

function ComponentsEditor() {
  const { project, run, patch } = useSettings()
  const people = usePeople().data ?? []
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [deleting, setDeleting] = useState<Project['components'][number] | null>(null)
  const update = (id: string, body: Record<string, unknown>) => {
    patch((p) => ({ ...p, components: p.components.map((c) => (c.id === id ? { ...c, ...body } : c)) }))
    run(() => api.patch(`/components/${id}`, body))
  }
  const lead = (id: string | null): Named | null => (id ? { id, name: people.find((p) => p.id === id)?.name ?? '…' } : null)
  return (
    <Section title={t('Components')} description={t('Parts of the product (like Checkout or API) to sort issues by. A component lead is a contact person, nothing more.')}>
      {project.components.length > 0 && (
        <div className="ps-list">
          {project.components.map((c) => (
            <div key={c.id} className="ps-row ps-comp-row">
              <Puzzle size={15} className="faint" aria-hidden />
              <InlineText className="ps-comp-name" value={c.name} label={t('Component name')} onSave={(v) => update(c.id, { name: v })} />
              <InlineText className="ps-grow" value={c.description ?? ''} required={false} placeholder={t('Description')} label={t('Description of {name}', { name: c.name })} onSave={(v) => update(c.id, { description: v || null })} />
              <span className="ps-comp-lead">
                <PersonPicker label={t('Component lead')} value={lead(c.leadId)} onChange={(id) => update(c.id, { leadId: id })} />
              </span>
              <button className="icon-btn" title={t('Delete')} aria-label={t('Delete {name}', { name: c.name })} onClick={() => setDeleting(c)}><Trash size={15} /></button>
            </div>
          ))}
        </div>
      )}
      <form
        className="ps-row ps-add-row"
        onSubmit={async (e) => {
          e.preventDefault()
          if (!name.trim()) return
          const ok = await run(() => api.post(`/projects/${project.key}/components`, { name: name.trim(), description: description.trim() || null }), t('Component {name} added', { name: name.trim() }))
          if (ok) {
            setName('')
            setDescription('')
          }
        }}
      >
        <input className="input input-sm ps-comp-name" placeholder={t('New component')} aria-label={t('New component name')} value={name} onChange={(e) => setName(e.target.value)} />
        <input className="input input-sm ps-grow" placeholder={t('Description (optional)')} aria-label={t('New component description')} value={description} onChange={(e) => setDescription(e.target.value)} />
        <button className="btn btn-sm btn-primary" type="submit" disabled={!name.trim()}><Plus size={14} /> {t('Add component')}</button>
      </form>
      {deleting && (
        <Confirm
          title={t('Delete component {name}?', { name: deleting.name })}
          body={<p>{t('Issues in this component keep everything else but lose the component.')}</p>}
          confirmLabel={t('Delete component')}
          danger
          onConfirm={() => run(() => api.del(`/components/${deleting.id}`)).then(() => undefined)}
          onClose={() => setDeleting(null)}
        />
      )}
    </Section>
  )
}

// ---- boards ----

const CARD_FIELDS: { id: string; label: () => string }[] = [
  { id: 'priority', label: () => t('Priority') },
  { id: 'assignee', label: () => t('Assignee') },
  { id: 'points', label: () => t('Estimate') },
  { id: 'due', label: () => t('Due date') },
  { id: 'labels', label: () => t('Labels') },
  { id: 'comments', label: () => t('Comment count') },
]
const SWIMLANES: { id: BoardConfig['swimlanes']; label: () => string }[] = [
  { id: 'none', label: () => t('No swimlanes') },
  { id: 'assignee', label: () => t('By assignee') },
  { id: 'epic', label: () => t('By epic') },
  { id: 'priority', label: () => t('By priority') },
  { id: 'type', label: () => t('By type') },
]

function BoardsSection({ boardId }: { boardId?: string }) {
  const { project } = useSettings()
  const nav = useNavigate()
  const [creating, setCreating] = useState(false)
  const current = project.boards.find((b) => b.id === boardId) ?? project.boards[0]
  return (
    <>
      <Section
        title={t('Boards')}
        description={t('Which statuses show in which column, WIP limits, quick filters and what cards show.')}
        actions={<button className="btn btn-primary" onClick={() => setCreating(true)}><Plus size={15} /> {t('New board')}</button>}
      >
        {project.boards.length > 1 && (
          <div className="ps-board-tabs" role="tablist">
            {project.boards.map((b) => (
              <button
                key={b.id}
                role="tab"
                aria-selected={b.id === current?.id}
                className={`chip ${b.id === current?.id ? 'on' : ''}`}
                onClick={() => nav(`/projects/${project.key}/settings/boards/${b.id}`)}
              >
                <Kanban size={14} aria-hidden /> {b.name}
              </button>
            ))}
          </div>
        )}
      </Section>
      {current ? <BoardEditor key={current.id} boardId={current.id} /> : <Empty icon={<Kanban size={30} />} title={t('No boards yet')} />}
      {creating && <CreateBoardDialog onClose={() => setCreating(false)} />}
    </>
  )
}

function CreateBoardDialog({ onClose }: { onClose: () => void }) {
  const { project, refresh } = useSettings()
  const nav = useNavigate()
  const toast = useToast()
  const [name, setName] = useState('')
  const [kind, setKind] = useState<'kanban' | 'scrum'>(project.kind === 'scrum' ? 'scrum' : 'kanban')
  const [filter, setFilter] = useState('')
  const [valid, setValid] = useState(true)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  return (
    <Modal
      title={t('New board')}
      onClose={onClose}
      width={520}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button
            className="btn btn-primary"
            disabled={busy || !name.trim() || !valid}
            onClick={async () => {
              setBusy(true)
              setErr('')
              try {
                const r = await api.post<{ id: string }>(`/projects/${project.key}/boards`, { name: name.trim(), kind, filter })
                refresh()
                toast(t('Board {name} created', { name: name.trim() }))
                onClose()
                nav(`/projects/${project.key}/settings/boards/${r.id}`)
              } catch (e) {
                setErr(errText(e))
              } finally {
                setBusy(false)
              }
            }}
          >
            {t('Create board')}
          </button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="ps-board-name">{t('Name')}</label>
        <input id="ps-board-name" className="input" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder={t('e.g. Design board')} />
      </div>
      <div className="field">
        <span className="field-label">{t('Kind')}</span>
        <div className="seg" role="radiogroup" aria-label={t('Kind')}>
          <button type="button" role="radio" aria-checked={kind === 'kanban'} className={kind === 'kanban' ? 'active' : ''} onClick={() => setKind('kanban')}>{t('Kanban')}</button>
          <button type="button" role="radio" aria-checked={kind === 'scrum'} className={kind === 'scrum' ? 'active' : ''} onClick={() => setKind('scrum')}>{t('Scrum')}</button>
        </div>
        <span className="field-hint">{kind === 'scrum' ? t('Shows the issues of the active sprint.') : t('Shows all matching issues in a continuous flow.')}</span>
      </div>
      <div className="field">
        <label htmlFor="ps-board-filter">{t('Filter (optional, TQL)')}</label>
        <TqlInput id="ps-board-filter" value={filter} onChange={setFilter} onValidity={setValid} placeholder={t('e.g. component = Design')} />
        <span className="field-hint">{t('Only issues matching this query appear. Columns start as one per status.')}</span>
      </div>
      {err && <p className="error-text">{err}</p>}
    </Modal>
  )
}

function BoardEditor({ boardId }: { boardId: string }) {
  const q = useQuery({ queryKey: ['board-config', boardId], queryFn: () => api.get<BoardData>(`/boards/${boardId}`) })
  if (q.isLoading) return <Loading />
  if (q.error) return <ErrorBox error={q.error} />
  if (!q.data) return null
  return <BoardForm data={q.data} />
}

type DraftFilter = { k: string; name: string; query: string }
type BoardDraft = { name: string; columns: BoardColumn[]; quickFilters: DraftFilter[]; cardFields: string[]; swimlanes: BoardConfig['swimlanes']; filter: string; hideDoneAfterDays: number }
/** A board draft for comparing (without the local quick-filter keys). */
const boardSig = (d: BoardDraft) => JSON.stringify({ ...d, quickFilters: d.quickFilters.map((q) => [q.name, q.query]) })

function BoardForm({ data }: { data: BoardData }) {
  const { project, run, refresh } = useSettings()
  const qc = useQueryClient()
  const nav = useNavigate()
  const cfg = data.board.config
  const statuses = project.statuses
  const init = useMemo(
    (): BoardDraft => ({
      name: data.board.name,
      columns: (cfg.columns ?? []).map((c) => ({ ...c, statusIds: c.statusIds.filter((id) => statuses.some((s) => s.id === id)) })),
      quickFilters: (cfg.quickFilters ?? []).map((f, i) => ({ k: `q${i}`, name: f.name, query: f.query })),
      cardFields: cfg.cardFields ?? ['priority', 'assignee', 'points', 'due', 'labels'],
      swimlanes: cfg.swimlanes ?? 'none',
      filter: cfg.filter ?? '',
      hideDoneAfterDays: cfg.hideDoneAfterDays ?? 14,
    }),
    [data, cfg, statuses],
  )
  const [f, setF] = useState(init)
  useFollow(init, setF, boardSig)
  const [invalid, setInvalid] = useState<Record<string, boolean>>({})
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const [deleting, setDeleting] = useState(false)
  const set = <K extends keyof typeof f>(k: K, v: (typeof f)[K]) => setF((x) => ({ ...x, [k]: v }))
  const setCol = (id: string, v: Partial<BoardColumn>) => setF((x) => ({ ...x, columns: x.columns.map((c) => (c.id === id ? { ...c, ...v } : c)) }))
  const validity = useCallback((k: string) => (ok: boolean) => setInvalid((m) => (m[k] === !ok ? m : { ...m, [k]: !ok })), [])
  const dirty = boardSig(f) !== boardSig(init)
  const mapped = new Map<string, string>()
  f.columns.forEach((c) => c.statusIds.forEach((s) => mapped.set(s, c.id)))
  const unmapped = statuses.filter((s) => !mapped.has(s.id))
  const hasErrors = Object.values(invalid).some(Boolean) || f.columns.length === 0 || f.columns.some((c) => !c.name.trim()) || !f.name.trim()

  const assign = (statusId: string, colId: string) =>
    setF((x) => ({
      ...x,
      columns: x.columns.map((c) => (c.id === colId ? { ...c, statusIds: [...c.statusIds.filter((s) => s !== statusId), statusId] } : { ...c, statusIds: c.statusIds.filter((s) => s !== statusId) })),
    }))
  const save = async () => {
    setBusy(true)
    setErr('')
    try {
      const config: BoardConfig = {
        columns: f.columns.map((c) => ({ id: c.id, name: c.name.trim(), statusIds: c.statusIds, wipLimit: c.wipLimit && c.wipLimit > 0 ? c.wipLimit : null })),
        quickFilters: f.quickFilters.filter((q) => q.name.trim() && q.query.trim()).map((q) => ({ name: q.name.trim(), query: q.query.trim() })),
        cardFields: f.cardFields,
        swimlanes: f.swimlanes,
        filter: f.filter.trim(),
        hideDoneAfterDays: Math.max(0, Math.min(3650, Math.round(f.hideDoneAfterDays || 0))),
      }
      await api.put(`/boards/${data.board.id}`, { name: f.name.trim(), config })
      qc.invalidateQueries({ queryKey: ['board-config', data.board.id] })
      refresh()
    } catch (e) {
      setErr(errText(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <Section
        title={
          <span className="row">
            <Kanban size={17} aria-hidden /> {data.board.name}
            <span className="badge">{data.board.kind === 'scrum' ? t('Scrum') : t('Kanban')}</span>
          </span>
        }
        actions={
          <>
            <Link className="btn" to={`/projects/${project.key}/board?board=${data.board.id}`}><ExternalLink size={14} /> {t('Open board')}</Link>
            <button className="btn btn-danger" disabled={project.boards.length <= 1} title={project.boards.length <= 1 ? t('A project keeps at least one board') : undefined} onClick={() => setDeleting(true)}>
              <Trash size={15} /> {t('Delete')}
            </button>
          </>
        }
      >
        <div className="ps-grid-2">
          <div className="field">
            <label htmlFor="ps-b-name">{t('Board name')}</label>
            <input id="ps-b-name" className="input" value={f.name} onChange={(e) => set('name', e.target.value)} />
          </div>
          <div className="field">
            <label htmlFor="ps-b-lanes">{t('Default swimlanes')}</label>
            <select id="ps-b-lanes" className="select" value={f.swimlanes} onChange={(e) => set('swimlanes', e.target.value as BoardConfig['swimlanes'])}>
              {SWIMLANES.map((s) => <option key={s.id} value={s.id}>{s.label()}</option>)}
            </select>
          </div>
        </div>
      </Section>

      <Section
        title={<span className="row"><Columns3 size={17} aria-hidden /> {t('Columns')}</span>}
        description={t('Drag to reorder. Each status can be in one column at most; a column can hold several statuses.')}
        actions={
          <button className="btn" onClick={() => set('columns', [...f.columns, { id: newId(), name: t('New column'), statusIds: [], wipLimit: null }])}>
            <Plus size={15} /> {t('Add column')}
          </button>
        }
      >
        <Sortable items={f.columns} onReorder={(ids) => set('columns', ids.map((id) => f.columns.find((c) => c.id === id)!).filter(Boolean))} className="ps-list">
          {(c, grip) => (
            <div className="ps-col">
              <div className="ps-row">
                {grip}
                <input className="input input-sm ps-grow" value={c.name} aria-label={t('Column name')} onChange={(e) => setCol(c.id, { name: e.target.value })} aria-invalid={!c.name.trim()} />
                <label className="ps-wip">
                  <span>{t('WIP limit')}</span>
                  <input
                    type="number"
                    min={0}
                    className="input input-sm"
                    value={c.wipLimit ?? ''}
                    placeholder="–"
                    onChange={(e) => setCol(c.id, { wipLimit: e.target.value === '' ? null : Math.max(0, Number(e.target.value)) })}
                  />
                </label>
                <button className="icon-btn" title={t('Remove column')} aria-label={t('Remove column {name}', { name: c.name })} onClick={() => set('columns', f.columns.filter((x) => x.id !== c.id))}>
                  <X size={15} />
                </button>
              </div>
              <div className="ps-col-statuses">
                {c.statusIds.length === 0 && <span className="faint small">{t('No statuses: the column stays empty')}</span>}
                {c.statusIds.map((sid) => {
                  const s = statuses.find((x) => x.id === sid)
                  if (!s) return null
                  return (
                    <span key={sid} className="ps-status-chip">
                      <Lozenge name={s.name} category={s.category} />
                      <button className="icon-btn icon-btn-sm" title={t('Remove from column')} aria-label={t('Remove {name} from the column', { name: s.name })} onClick={() => setCol(c.id, { statusIds: c.statusIds.filter((x) => x !== sid) })}>
                        <X size={13} />
                      </button>
                    </span>
                  )
                })}
                <PopButton className="btn btn-sm btn-ghost" width={260} title={t('Add a status to this column')} label={<><Plus size={13} /> {t('Status')}</>}>
                  {(close) => (
                    <Choices
                      items={statuses.filter((s) => !c.statusIds.includes(s.id))}
                      filterText={(s) => s.name}
                      empty={t('All statuses are in this column')}
                      render={(s) => {
                        const other = mapped.get(s.id)
                        return (
                          <>
                            <Lozenge name={s.name} category={s.category} />
                            {other && <span className="mi-desc">{t('moves from {col}', { col: f.columns.find((x) => x.id === other)?.name ?? '' })}</span>}
                          </>
                        )
                      }}
                      onPick={(s) => {
                        close()
                        assign(s.id, c.id)
                      }}
                    />
                  )}
                </PopButton>
              </div>
            </div>
          )}
        </Sortable>
        {unmapped.length > 0 && (
          <div className="ps-unmapped">
            <div className="ps-strong small">{t('Not on this board')}</div>
            <div className="faint small">{t('Issues in these statuses don’t show on the board. Add them to a column, or give them a column of their own.')}</div>
            <div className="ps-chips">
              {unmapped.map((s) => (
                <span key={s.id} className="ps-status-chip">
                  <Lozenge name={s.name} category={s.category} />
                  <button
                    className="btn btn-sm btn-ghost"
                    title={t('New column for {name}', { name: s.name })}
                    onClick={() => set('columns', [...f.columns, { id: newId(), name: s.name, statusIds: [s.id], wipLimit: null }])}
                  >
                    <Plus size={13} /> {t('Column')}
                  </button>
                </span>
              ))}
            </div>
          </div>
        )}
      </Section>

      <Section
        title={<span className="row"><Funnel size={17} aria-hidden /> {t('Filters')}</span>}
        description={t('The base filter limits which issues the board shows. Quick filters are one-click buttons above the board.')}
      >
        <div className="field">
          <label htmlFor="ps-b-filter">{t('Base filter (TQL)')}</label>
          <TqlInput id="ps-b-filter" value={f.filter} onChange={(v) => set('filter', v)} onValidity={validity('filter')} placeholder={t('All issues of the project')} />
        </div>
        <span className="field-label">{t('Quick filters')}</span>
        <div className="ps-list ps-list-tight">
          {f.quickFilters.map((qf) => (
            <div key={qf.k} className="ps-row ps-align-start ps-qf-row">
              <input
                className="input input-sm ps-qf-name"
                value={qf.name}
                placeholder={t('Name')}
                aria-label={t('Quick filter name')}
                onChange={(e) => set('quickFilters', f.quickFilters.map((x) => (x.k === qf.k ? { ...x, name: e.target.value } : x)))}
              />
              <div className="ps-grow">
                <TqlInput small value={qf.query} onValidity={validity(qf.k)} placeholder={t('e.g. assignee = me()')} onChange={(v) => set('quickFilters', f.quickFilters.map((x) => (x.k === qf.k ? { ...x, query: v } : x)))} />
              </div>
              <button
                className="icon-btn"
                title={t('Remove quick filter')}
                aria-label={t('Remove quick filter {name}', { name: qf.name })}
                onClick={() => {
                  set('quickFilters', f.quickFilters.filter((x) => x.k !== qf.k))
                  validity(qf.k)(true)
                }}
              >
                <X size={15} />
              </button>
            </div>
          ))}
        </div>
        <button className="btn btn-sm" onClick={() => set('quickFilters', [...f.quickFilters, { k: newId(), name: '', query: '' }])}><Plus size={14} /> {t('Add quick filter')}</button>
      </Section>

      <Section title={t('Cards')} description={t('What each card shows besides its key, type and title.')}>
        <div className="ps-checks">
          {CARD_FIELDS.map((c) => (
            <label key={c.id} className="ps-check">
              <input
                type="checkbox"
                checked={f.cardFields.includes(c.id)}
                onChange={(e) => set('cardFields', e.target.checked ? [...f.cardFields, c.id] : f.cardFields.filter((x) => x !== c.id))}
              />
              {c.label()}
            </label>
          ))}
        </div>
        <div className="field ps-mt-12">
          <label htmlFor="ps-b-days">{t('Keep done issues for (days)')}</label>
          <input
            id="ps-b-days"
            type="number"
            min={0}
            max={3650}
            className="input ps-num"
            value={f.hideDoneAfterDays}
            disabled={data.board.kind === 'scrum'}
            onChange={(e) => set('hideDoneAfterDays', Number(e.target.value))}
          />
          <span className="field-hint">
            {data.board.kind === 'scrum' ? t('Scrum boards show the whole sprint, done issues included.') : t('Done issues leave the board this many days after they were resolved.')}
          </span>
        </div>
      </Section>

      <SaveBar
        dirty={dirty}
        busy={busy}
        error={err}
        disabled={hasErrors}
        onSave={save}
        onReset={() => {
          setF(init)
          setErr('')
          setInvalid({})
        }}
      />
      {deleting && (
        <Confirm
          title={t('Delete board {name}?', { name: data.board.name })}
          body={<p>{t('Only the board goes away; its issues stay in the project.')}</p>}
          confirmLabel={t('Delete board')}
          danger
          onConfirm={async () => {
            const ok = await run(() => api.del(`/boards/${data.board.id}`), t('Board deleted'))
            if (ok) nav(`/projects/${project.key}/settings/boards`, { replace: true })
          }}
          onClose={() => setDeleting(false)}
        />
      )}
    </>
  )
}

// ---- automation ----

const EVENTS: { id: TriggerEvent; label: () => string }[] = [
  { id: 'created', label: () => t('an issue is created') },
  { id: 'status', label: () => t('an issue changes status') },
  { id: 'assigned', label: () => t('an issue is assigned') },
  { id: 'field', label: () => t('a field changes') },
  { id: 'commented', label: () => t('someone comments') },
  { id: 'schedule', label: () => t('on a schedule') },
]
const TRIGGER_FIELDS: { id: string; label: () => string }[] = [
  { id: 'title', label: () => t('Title') },
  { id: 'description', label: () => t('Description') },
  { id: 'type', label: () => t('Type') },
  { id: 'priority', label: () => t('Priority') },
  { id: 'assignee', label: () => t('Assignee') },
  { id: 'reporter', label: () => t('Reporter') },
  { id: 'parent', label: () => t('Parent') },
  { id: 'sprint', label: () => t('Sprint') },
  { id: 'version', label: () => t('Release') },
  { id: 'component', label: () => t('Component') },
  { id: 'points', label: () => t('Story points') },
  { id: 'estimate', label: () => t('Estimate') },
  { id: 'start', label: () => t('Start date') },
  { id: 'due', label: () => t('Due date') },
  { id: 'labels', label: () => t('Labels') },
  { id: 'custom', label: () => t('Any custom field') },
]
const ACTIONS: { id: ActionKind; label: () => string }[] = [
  { id: 'setStatus', label: () => t('move it to a status') },
  { id: 'assign', label: () => t('assign it') },
  { id: 'setPriority', label: () => t('set the priority') },
  { id: 'addLabel', label: () => t('add a label') },
  { id: 'removeLabel', label: () => t('remove a label') },
  { id: 'setSprint', label: () => t('move it to a sprint') },
  { id: 'comment', label: () => t('add a comment') },
  { id: 'notify', label: () => t('notify someone') },
]

function describeTrigger(tr: Trigger, project: Project): string {
  const st = (id?: string) => project.statuses.find((s) => s.id === id)?.name ?? '?'
  switch (tr.event) {
    case 'created':
      return t('an issue is created')
    case 'assigned':
      return t('an issue is assigned')
    case 'commented':
      return t('someone comments on an issue')
    case 'schedule':
      return describeSchedule(tr.schedule)
    case 'field':
      return t('the field “{field}” changes', { field: TRIGGER_FIELDS.find((f) => f.id === tr.field)?.label() ?? tr.field ?? '?' })
    case 'status': {
      const to = tr.toStatusId ? st(tr.toStatusId) : tr.toCategory ? t('any “{cat}” status', { cat: catLabel(tr.toCategory) }) : ''
      if (tr.fromStatusId && to) return t('an issue moves from {from} to {to}', { from: st(tr.fromStatusId), to })
      if (tr.fromStatusId) return t('an issue moves out of {from}', { from: st(tr.fromStatusId) })
      if (to) return t('an issue moves to {to}', { to })
      return t('an issue changes status')
    }
  }
  return ''
}

function describeAction(a: RuleAction, project: Project, people: Named[]): string {
  switch (a.action) {
    case 'setStatus':
      return t('move it to {status}', { status: project.statuses.find((s) => s.id === a.statusId)?.name ?? '?' })
    case 'assign':
      return a.to === 'reporter' ? t('assign it to the reporter') : a.to === 'nobody' || !a.userId ? t('unassign it') : t('assign it to {name}', { name: people.find((p) => p.id === a.userId)?.name ?? '?' })
    case 'setPriority':
      return t('set the priority to {priority}', { priority: priorityLabel(a.priority ?? 'medium') })
    case 'addLabel':
      return t('add the label {label}', { label: a.label || '?' })
    case 'removeLabel':
      return t('remove the label {label}', { label: a.label || '?' })
    case 'setSprint':
      return a.sprint === 'active' ? t('move it to the active sprint') : t('move it to the backlog')
    case 'comment':
      return t('add the comment “{text}”', { text: (a.text ?? '').length > 60 ? (a.text ?? '').slice(0, 57) + '…' : a.text ?? '' })
    case 'notify':
      return t('notify {who}', { who: a.to === 'reporter' ? t('the reporter') : a.to === 'watchers' ? t('the watchers') : a.to === 'lead' ? t('the project lead') : t('the assignee') })
  }
  return ''
}

function joinList(items: string[]): string {
  if (items.length <= 1) return items[0] ?? ''
  return t('{list} and {last}', { list: items.slice(0, -1).join(', '), last: items[items.length - 1] })
}

function describeRule(r: { trigger: Trigger; condition: string; actions: RuleAction[] }, project: Project, people: Named[]): string {
  const when = describeTrigger(r.trigger, project)
  const then = joinList(r.actions.map((a) => describeAction(a, project, people)))
  if (r.trigger.event === 'schedule')
    return t('{schedule}, for every issue matching {query}: {actions}.', { schedule: when.charAt(0).toUpperCase() + when.slice(1), query: r.condition.trim() || '…', actions: then })
  return r.condition.trim()
    ? t('When {trigger} and it matches {query}, then {actions}.', { trigger: when, query: r.condition.trim(), actions: then })
    : t('When {trigger}, then {actions}.', { trigger: when, actions: then })
}

function AutomationSection() {
  const { project } = useSettings()
  const qc = useQueryClient()
  const toast = useToast()
  const people = usePeople().data ?? []
  const qk = ['automation', project.key]
  const rules = useQuery({ queryKey: qk, queryFn: () => api.get<Rule[]>(`/projects/${project.key}/automation`) })
  const [editing, setEditing] = useState<Rule | 'new' | null>(null)
  const [deleting, setDeleting] = useState<Rule | null>(null)
  const toggle = async (r: Rule) => {
    qc.setQueryData<Rule[]>(qk, (old) => old?.map((x) => (x.id === r.id ? { ...x, enabled: !r.enabled } : x)))
    try {
      await api.patch(`/automation/${r.id}`, { enabled: !r.enabled })
      toast(r.enabled ? t('Rule paused') : t('Rule turned on'))
    } catch (e) {
      toast(errText(e))
    }
    qc.invalidateQueries({ queryKey: qk })
  }
  return (
    <Section
      title={t('Automation')}
      description={t('Rules that act on issues for you: when something happens and the issue matches, the rule makes changes as “Automation”.')}
      actions={<button className="btn btn-primary" onClick={() => setEditing('new')}><Plus size={15} /> {t('New rule')}</button>}
    >
      {rules.isLoading && <Loading />}
      {rules.error && <ErrorBox error={rules.error} />}
      {rules.data && rules.data.length === 0 && (
        <Empty icon={<Bot size={32} />} title={t('No rules yet')}>
          {t('For example: when a bug is created, add the label triage; when an issue moves to Done, unassign it.')}
        </Empty>
      )}
      <div className="ps-rules">
        {rules.data?.map((r) => (
          <div key={r.id} className={`ps-rule ${r.enabled ? '' : 'off'}`}>
            <label className="ps-switch" title={r.enabled ? t('On') : t('Off')}>
              <input type="checkbox" checked={r.enabled} onChange={() => toggle(r)} aria-label={t('Rule {name} is on', { name: r.name })} />
              <span aria-hidden />
            </label>
            <div className="ps-rule-main">
              <button className="ps-link-btn ps-strong" onClick={() => setEditing(r)}>{r.name}</button>
              <div className="ps-rule-sentence">{describeRule(r, project, people)}</div>
              <div className="faint small">
                {r.runs > 0 || r.lastRunAt
                  ? t('Ran {count} times, last {when}', { count: r.runs, when: r.lastRunAt ? ago(r.lastRunAt) : t('never') })
                  : t('Hasn’t run yet')}
                {r.enabled && r.nextRunAt && <> · {t('next run {when}', { when: dateTime(r.nextRunAt) })}</>}
                {!r.enabled && <> · {t('paused')}</>}
              </div>
            </div>
            <button className="icon-btn" title={t('Edit')} aria-label={t('Edit {name}', { name: r.name })} onClick={() => setEditing(r)}><Pencil size={15} /></button>
            <button className="icon-btn" title={t('Delete')} aria-label={t('Delete {name}', { name: r.name })} onClick={() => setDeleting(r)}><Trash size={15} /></button>
          </div>
        ))}
      </div>
      {editing && <RuleDialog rule={editing === 'new' ? null : editing} onClose={() => setEditing(null)} />}
      {deleting && (
        <Confirm
          title={t('Delete rule {name}?', { name: deleting.name })}
          body={<p>{t('Changes the rule already made stay. To stop it for a while, turn it off instead.')}</p>}
          confirmLabel={t('Delete rule')}
          danger
          onConfirm={async () => {
            try {
              await api.del(`/automation/${deleting.id}`)
              toast(t('Rule deleted'))
            } catch (e) {
              toast(errText(e))
            }
            qc.invalidateQueries({ queryKey: qk })
          }}
          onClose={() => setDeleting(null)}
        />
      )}
    </Section>
  )
}

function actionReady(a: RuleAction): boolean {
  switch (a.action) {
    case 'setStatus':
      return !!a.statusId
    case 'assign':
      return a.to === 'reporter' || a.to === 'nobody' || !!a.userId
    case 'addLabel':
    case 'removeLabel':
      return !!a.label?.trim()
    case 'comment':
      return !!a.text?.trim()
    case 'notify':
      return !!a.message?.trim()
    default:
      return true
  }
}

function cleanAction(a: RuleAction): RuleAction {
  switch (a.action) {
    case 'setStatus':
      return { action: a.action, statusId: a.statusId }
    case 'assign':
      return a.to ? { action: a.action, to: a.to } : { action: a.action, userId: a.userId }
    case 'setPriority':
      return { action: a.action, priority: a.priority ?? 'medium' }
    case 'addLabel':
    case 'removeLabel':
      return { action: a.action, label: (a.label ?? '').trim() }
    case 'setSprint':
      return { action: a.action, sprint: a.sprint === 'active' ? 'active' : null }
    case 'comment':
      return { action: a.action, text: a.text ?? '' }
    case 'notify':
      return { action: a.action, to: a.to ?? 'assignee', message: a.message ?? '' }
  }
}

function newAction(kind: ActionKind, project: Project): RuleAction {
  switch (kind) {
    case 'setStatus':
      return { action: kind, statusId: project.statuses[0]?.id }
    case 'assign':
      return { action: kind, to: 'reporter' }
    case 'setPriority':
      return { action: kind, priority: 'high' }
    case 'setSprint':
      return { action: kind, sprint: 'active' }
    case 'notify':
      return { action: kind, to: 'assignee', message: '' }
    default:
      return { action: kind, label: '', text: '' }
  }
}

function RuleDialog({ rule, onClose }: { rule: Rule | null; onClose: () => void }) {
  const { project } = useSettings()
  const qc = useQueryClient()
  const toast = useToast()
  const people = usePeople().data ?? []
  const [name, setName] = useState(rule?.name ?? '')
  const [trigger, setTrigger] = useState<Trigger>(rule?.trigger ?? { event: 'created' })
  const [condition, setCondition] = useState(rule?.condition ?? '')
  const [condOk, setCondOk] = useState(true)
  const [actions, setActions] = useState<(RuleAction & { k: string })[]>(() => (rule?.actions ?? [{ action: 'addLabel', label: '' }]).map((a) => ({ ...a, k: newId() })))
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const setAct = (k: string, a: RuleAction) => setActions((l) => l.map((x) => (x.k === k ? { ...a, k } : x)))
  const toValue = trigger.toStatusId ? `s:${trigger.toStatusId}` : trigger.toCategory ? `c:${trigger.toCategory}` : ''
  const ready =
    actions.length > 0 && actions.every(actionReady) && condOk && (trigger.event !== 'field' || !!trigger.field) && (trigger.event !== 'schedule' || !!condition.trim())
  const cleanTrigger = (): Trigger => {
    const tr: Trigger = { event: trigger.event }
    if (trigger.event === 'status') {
      if (trigger.fromStatusId) tr.fromStatusId = trigger.fromStatusId
      if (trigger.toStatusId) tr.toStatusId = trigger.toStatusId
      else if (trigger.toCategory) tr.toCategory = trigger.toCategory
    }
    if (trigger.event === 'field') tr.field = trigger.field
    if (trigger.event === 'schedule') tr.schedule = trigger.schedule ?? defaultSchedule()
    return tr
  }
  const draft = { trigger: cleanTrigger(), condition, actions: actions.map(cleanAction) }
  const submit = async () => {
    setBusy(true)
    setErr('')
    const body = { name: name.trim() || describeRule(draft, project, people).slice(0, 80), ...draft }
    try {
      if (rule) await api.patch(`/automation/${rule.id}`, body)
      else await api.post(`/projects/${project.key}/automation`, { ...body, enabled: true })
      qc.invalidateQueries({ queryKey: ['automation', project.key] })
      toast(rule ? t('Rule saved') : t('Rule created'))
      onClose()
    } catch (e) {
      setErr(errText(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal
      title={rule ? t('Edit rule') : t('New automation rule')}
      onClose={onClose}
      width={680}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !ready} onClick={submit}>{rule ? t('Save rule') : t('Create rule')}</button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="ps-rule-name">{t('Name')}</label>
        <input id="ps-rule-name" className="input" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder={t('e.g. New bugs get the triage label')} />
      </div>
      <div className="ps-builder">
        <div className="ps-builder-step">
          <span className="ps-builder-word">{t('When')}</span>
          <div className="ps-builder-body">
            <select className="select input-sm ps-auto" aria-label={t('Trigger')} value={trigger.event} onChange={(e) => setTrigger(e.target.value === 'schedule' ? { event: 'schedule', schedule: trigger.schedule ?? defaultSchedule() } : { event: e.target.value as TriggerEvent })}>
              {EVENTS.map((ev) => <option key={ev.id} value={ev.id}>{ev.label()}</option>)}
            </select>
            {trigger.event === 'status' && (
              <>
                <span className="ps-builder-joint">{t('from')}</span>
                <select className="select input-sm ps-auto" aria-label={t('From status')} value={trigger.fromStatusId ?? ''} onChange={(e) => setTrigger({ ...trigger, fromStatusId: e.target.value || undefined })}>
                  <option value="">{t('any status')}</option>
                  {project.statuses.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
                </select>
                <span className="ps-builder-joint">{t('to')}</span>
                <select
                  className="select input-sm ps-auto"
                  aria-label={t('To status')}
                  value={toValue}
                  onChange={(e) => {
                    const v = e.target.value
                    setTrigger({ ...trigger, toStatusId: v.startsWith('s:') ? v.slice(2) : undefined, toCategory: v.startsWith('c:') ? (v.slice(2) as Category) : undefined })
                  }}
                >
                  <option value="">{t('any status')}</option>
                  <optgroup label={t('Category')}>
                    {CATEGORIES.map((c) => <option key={c} value={`c:${c}`}>{t('any “{cat}” status', { cat: catLabel(c) })}</option>)}
                  </optgroup>
                  <optgroup label={t('Status')}>
                    {project.statuses.map((s) => <option key={s.id} value={`s:${s.id}`}>{s.name}</option>)}
                  </optgroup>
                </select>
              </>
            )}
            {trigger.event === 'schedule' && <ScheduleEditor value={trigger.schedule ?? defaultSchedule()} onChange={(sc) => setTrigger({ ...trigger, schedule: sc })} />}
            {trigger.event === 'field' && (
              <select className="select input-sm ps-auto" aria-label={t('Field')} value={trigger.field ?? ''} onChange={(e) => setTrigger({ ...trigger, field: e.target.value || undefined })}>
                <option value="">{t('Pick a field…')}</option>
                {TRIGGER_FIELDS.map((f) => <option key={f.id} value={f.id}>{f.label()}</option>)}
              </select>
            )}
          </div>
        </div>
        <div className="ps-builder-step">
          <span className="ps-builder-word">{trigger.event === 'schedule' ? t('for issues matching') : t('and it matches')}</span>
          <div className="ps-builder-body ps-grow">
            <TqlInput
              small
              value={condition}
              onChange={setCondition}
              onValidity={setCondOk}
              placeholder={trigger.event === 'schedule' ? t('required, e.g. due <= today() and category != done') : t('any issue (optional TQL, e.g. type = Bug)')}
            />
          </div>
        </div>
        <div className="ps-builder-step">
          <span className="ps-builder-word">{t('then')}</span>
          <div className="ps-builder-body ps-builder-actions">
            {actions.map((a, i) => (
              <div key={a.k} className="ps-action">
                {i > 0 && <span className="ps-builder-joint">{t('and')}</span>}
                <select className="select input-sm ps-auto" aria-label={t('Action')} value={a.action} onChange={(e) => setAct(a.k, newAction(e.target.value as ActionKind, project))}>
                  {ACTIONS.map((x) => <option key={x.id} value={x.id}>{x.label()}</option>)}
                </select>
                <ActionParams a={a} onChange={(n) => setAct(a.k, n)} />
                {actions.length > 1 && (
                  <button className="icon-btn" title={t('Remove action')} aria-label={t('Remove action')} onClick={() => setActions((l) => l.filter((x) => x.k !== a.k))}><X size={15} /></button>
                )}
              </div>
            ))}
            {actions.length < 10 && (
              <button className="btn btn-sm btn-ghost ps-self-start" onClick={() => setActions((l) => [...l, { ...newAction('setPriority', project), k: newId() }])}>
                <Plus size={14} /> {t('Add action')}
              </button>
            )}
          </div>
        </div>
      </div>
      <div className="ps-rule-preview" aria-live="polite">
        <Bot size={16} aria-hidden />
        <span>{describeRule(draft, project, people)}</span>
      </div>
      {err && <p className="error-text">{err}</p>}
    </Modal>
  )
}

function ActionParams({ a, onChange }: { a: RuleAction; onChange: (a: RuleAction) => void }) {
  const { project } = useSettings()
  const people = usePeople().data ?? []
  switch (a.action) {
    case 'setStatus':
      return (
        <select className="select input-sm ps-auto" aria-label={t('Status')} value={a.statusId ?? ''} onChange={(e) => onChange({ ...a, statusId: e.target.value })}>
          {project.statuses.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
        </select>
      )
    case 'assign':
      return (
        <select
          className="select input-sm ps-auto"
          aria-label={t('Assign to')}
          value={a.to ?? (a.userId ? `u:${a.userId}` : 'nobody')}
          onChange={(e) => {
            const v = e.target.value
            onChange(v.startsWith('u:') ? { action: 'assign', userId: v.slice(2) } : { action: 'assign', to: v as 'reporter' | 'nobody' })
          }}
        >
          <option value="reporter">{t('to the reporter')}</option>
          <option value="nobody">{t('to nobody (unassign)')}</option>
          <optgroup label={t('People')}>
            {people.map((p) => <option key={p.id} value={`u:${p.id}`}>{p.name}</option>)}
          </optgroup>
        </select>
      )
    case 'setPriority':
      return (
        <select className="select input-sm ps-auto" aria-label={t('Priority')} value={a.priority ?? 'medium'} onChange={(e) => onChange({ ...a, priority: e.target.value as Priority })}>
          {PRIORITIES.map((p) => <option key={p} value={p}>{priorityLabel(p)}</option>)}
        </select>
      )
    case 'addLabel':
    case 'removeLabel':
      return <input className="input input-sm ps-label-input" aria-label={t('Label')} placeholder={t('label')} value={a.label ?? ''} onChange={(e) => onChange({ ...a, label: e.target.value.replace(/\s+/g, '-') })} />
    case 'setSprint':
      return (
        <select className="select input-sm ps-auto" aria-label={t('Sprint')} value={a.sprint ?? ''} onChange={(e) => onChange({ ...a, sprint: e.target.value === 'active' ? 'active' : null })}>
          <option value="active">{t('the active sprint')}</option>
          <option value="">{t('the backlog (no sprint)')}</option>
        </select>
      )
    case 'comment':
      return <textarea className="textarea ps-comment-input" rows={2} aria-label={t('Comment (Markdown)')} placeholder={t('Comment text (Markdown)')} value={a.text ?? ''} onChange={(e) => onChange({ ...a, text: e.target.value })} />
    case 'notify':
      return (
        <>
          <select className="select input-sm ps-auto" aria-label={t('Whom to notify')} value={a.to ?? 'assignee'} onChange={(e) => onChange({ ...a, to: e.target.value as NotifyTo })}>
            <option value="assignee">{t('the assignee')}</option>
            <option value="reporter">{t('the reporter')}</option>
            <option value="watchers">{t('the watchers')}</option>
            <option value="lead">{t('the project lead')}</option>
          </select>
          <input className="input input-sm ps-grow" aria-label={t('Message')} placeholder={t('Message, e.g. This is due tomorrow')} value={a.message ?? ''} onChange={(e) => onChange({ ...a, message: e.target.value })} />
        </>
      )
  }
}

// ---- integrations ----

function IntegrationsSection() {
  const { project } = useSettings()
  const me = useMe().data
  const wc = me?.wikicraft
  return (
    <Section
      title={<span className="row"><BookOpen size={17} aria-hidden /> {t('WikiCraft')}</span>}
      description={t('Link the project to a wiki space: the Docs page lists its pages, and specs, sprint reviews and release notes are written there.')}
    >
      {wc?.connected ? (
        <WikiSettings key={`${project.settings.wikiSpace ?? ''}|${project.settings.wikiParentPage ?? ''}`} />
      ) : (
        <div className="ps-note ps-note-block">
          <Plug size={18} aria-hidden />
          <div>
            <div className="ps-strong">{wc?.url ? t('WikiCraft isn’t connected for you') : t('WikiCraft isn’t connected')}</div>
            <p className="ps-mt-4">
              {wc?.url
                ? t('This workspace knows WikiCraft at {url}, but your account has no access token. Add a personal WikiCraft token under Profile & settings.', { url: wc.url })
                : t('A server admin can connect WikiCraft for everyone by setting TASKCRAFT_WIKICRAFT_URL and TASKCRAFT_WIKICRAFT_TOKEN. Or connect your own account with a personal token under Profile & settings.')}
            </p>
            <Link className="btn btn-sm" to="/settings">{t('Open Profile & settings')}</Link>
            {project.settings.wikiSpace && <p className="faint small">{t('The project is set to the space {space}.', { space: project.settings.wikiSpace })}</p>}
          </div>
        </div>
      )}
    </Section>
  )
}

function WikiSettings() {
  const { project, run } = useSettings()
  const spaces = useQuery({ queryKey: ['wiki-spaces'], queryFn: () => api.get<WikiSpace[]>('/integrations/wikicraft/spaces'), staleTime: 60_000 })
  const baseSpace = project.settings.wikiSpace ?? ''
  const baseParent = (project.settings.wikiParentPage as string | undefined) ?? ''
  const [space, setSpace] = useState(baseSpace)
  const [parent, setParent] = useState(baseParent)
  const [parentTitle, setParentTitle] = useState('')
  const [q, setQ] = useState('')
  const [debounced, setDebounced] = useState('')
  const [busy, setBusy] = useState(false)
  useEffect(() => {
    const h = setTimeout(() => setDebounced(q.trim()), 250)
    return () => clearTimeout(h)
  }, [q])
  const found = useQuery({
    queryKey: ['wiki-search', space, debounced],
    queryFn: () => api.get<WikiPage[]>(`/integrations/wikicraft/search?q=${encodeURIComponent(debounced)}&space=${encodeURIComponent(space)}`),
    enabled: !!space && !!debounced,
  })
  const dirty = space !== baseSpace || parent.trim() !== baseParent
  const parentOk = !parent.trim() || /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(parent.trim())
  const current = spaces.data?.find((s) => s.key === space)
  return (
    <>
      {spaces.error && <ErrorBox error={spaces.error} />}
      <div className="ps-grid-2">
        <div className="field">
          <label htmlFor="ps-wiki-space">{t('Wiki space')}</label>
          <select
            id="ps-wiki-space"
            className="select"
            value={space}
            onChange={(e) => {
              setSpace(e.target.value)
              if (e.target.value !== baseSpace) {
                setParent('')
                setParentTitle('')
              }
            }}
          >
            <option value="">{t('None')}</option>
            {spaces.data?.map((s) => <option key={s.key} value={s.key}>{s.name} ({s.key})</option>)}
            {space && spaces.data && !current && <option value={space}>{space}</option>}
          </select>
          {current && (
            <a className="field-hint" href={current.url} target="_blank" rel="noreferrer noopener">
              {t('Open {name} in WikiCraft', { name: current.name })} <ExternalLink size={11} />
            </a>
          )}
        </div>
        <div className="field">
          <label htmlFor="ps-wiki-parent">{t('Parent page for new pages (optional)')}</label>
          <input id="ps-wiki-parent" className="input ps-mono" value={parent} aria-invalid={!parentOk} placeholder={t('Page id')} onChange={(e) => { setParent(e.target.value); setParentTitle('') }} disabled={!space} />
          <span className="field-hint">{parentOk ? (parentTitle ? t('Under “{title}”', { title: parentTitle }) : t('New pages go under this page instead of the top of the space.')) : t('That isn’t a page id')}</span>
        </div>
      </div>
      {space && (
        <div className="field">
          <label htmlFor="ps-wiki-find">{t('Find the parent page')}</label>
          <div className="ps-search">
            <Search size={15} aria-hidden />
            <input id="ps-wiki-find" className="input" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t('Search pages in {space}…', { space: current?.name ?? space })} />
          </div>
          {debounced && (
            <div className="ps-wiki-results">
              {found.isLoading && <Loading />}
              {found.error && <span className="error-text small">{errText(found.error)}</span>}
              {found.data?.length === 0 && <span className="faint small">{t('No pages found')}</span>}
              {found.data?.slice(0, 8).map((p) => (
                <button
                  key={p.id}
                  className={`menu-item ${p.id === parent ? 'selected' : ''}`}
                  onClick={() => {
                    setParent(p.id)
                    setParentTitle(p.title)
                    setQ('')
                  }}
                >
                  <BookOpen size={14} aria-hidden />
                  <span className="col ps-gap-0">
                    <span>{p.title}</span>
                    {p.excerpt && <span className="mi-desc ps-ellipsis">{p.excerpt}</span>}
                  </span>
                  {p.id === parent && <Check size={14} className="ps-ml-auto" />}
                </button>
              ))}
            </div>
          )}
        </div>
      )}
      <SaveBar
        dirty={dirty}
        busy={busy}
        disabled={!parentOk}
        onReset={() => {
          setSpace(baseSpace)
          setParent(baseParent)
          setParentTitle('')
        }}
        onSave={async () => {
          setBusy(true)
          await run(() => api.patch(`/projects/${project.key}`, { settings: { wikiSpace: space || null, wikiParentPage: (space && parent.trim()) || null } }), t('Wiki settings saved'))
          setBusy(false)
        }}
      />
    </>
  )
}

// ---- trash ----

function TrashSection() {
  const { project } = useSettings()
  const qc = useQueryClient()
  const toast = useToast()
  const open = useOpenIssue()
  const qk = ['project-trash', project.key]
  const trash = useQuery({ queryKey: qk, queryFn: () => api.get<TrashItem[]>(`/projects/${project.key}/trash`) })
  const [busy, setBusy] = useState<string | null>(null)
  const restore = async (it: TrashItem) => {
    setBusy(it.id)
    try {
      await api.post(`/issues/${it.key}/restore`)
      for (const k of ['issues', 'board', 'backlog', 'search', 'my-work', 'project', 'timeline', 'calendar', 'report']) qc.invalidateQueries({ queryKey: [k] })
      toast(t('{key} restored', { key: it.key }), { label: t('Open [[action]]'), run: () => open(it.key) })
    } catch (e) {
      toast(errText(e))
    } finally {
      setBusy(null)
      qc.invalidateQueries({ queryKey: qk })
    }
  }
  return (
    <Section title={t('Trash')} description={t('Deleted issues stay here until they’re restored. Restoring brings back the issue with its comments, history and links.')}>
      {trash.isLoading && <Loading />}
      {trash.error && <ErrorBox error={trash.error} />}
      {trash.data && trash.data.length === 0 && <Empty icon={<Trash size={30} />} title={t('The trash is empty')} />}
      {trash.data && trash.data.length > 0 && (
        <div className="ps-table-wrap">
          <table className="list ps-table">
            <thead>
              <tr>
                <th>{t('Issue')}</th>
                <th className="ps-hide-phone">{t('Deleted')}</th>
                <th aria-label={t('Actions')} />
              </tr>
            </thead>
            <tbody>
              {trash.data.map((it) => (
                <tr key={it.id}>
                  <td>
                    <div className="row ps-trash-title">
                      <span className="issue-key">{it.key}</span>
                      <span className="ps-ellipsis">{it.title}</span>
                    </div>
                  </td>
                  <td className="ps-hide-phone faint small" title={dateTime(it.deletedAt)}>
                    {it.deletedBy ? t('{when} by {name}', { when: ago(it.deletedAt), name: it.deletedBy }) : ago(it.deletedAt)}
                  </td>
                  <td className="ps-td-actions">
                    <button className="btn btn-sm" disabled={busy === it.id} onClick={() => restore(it)}><Undo2 size={14} /> {t('Restore')}</button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Section>
  )
}
