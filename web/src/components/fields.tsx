// Pickers for issue fields: status (respecting the workflow), people, priority, type, sprint,
// version, component, labels, dates, numbers, parent and custom fields.
import { t } from '../i18n'
import { useEffect, useState, type ReactNode } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Check, X, Plus } from 'lucide-react'
import { api, PRIORITIES, shortDate, type CustomField, type Issue, type Named, type Priority, type Project, type StatusRef } from '../api'
import { Avatar, Choices, Lozenge, PopButton, Unassigned, useMe, usePeople } from '../ui'
import { PriorityIcon, TypeIcon, priorityLabel } from '../icons'

/** Statuses an issue may move to (restricted workflows only allow configured moves). */
export function allowedStatuses(project: Project, from: string): StatusRef[] {
  if (project.settings.workflow !== 'restricted' || project.role === 'admin') return project.statuses
  const ok = new Set(project.transitions.filter((x) => x.from === from).map((x) => x.to))
  return project.statuses.filter((s) => s.id === from || ok.has(s.id))
}

export function StatusPicker({ project, status, onChange, disabled, compact }: { project: Project; status: StatusRef; onChange: (id: string) => void; disabled?: boolean; compact?: boolean }) {
  const options = allowedStatuses(project, status.id)
  return (
    <PopButton
      disabled={disabled}
      className={compact ? 'cell-btn' : `status-btn ${status.category}`}
      title={t('Status')}
      label={compact ? <Lozenge name={status.name} category={status.category} /> : <>{status.name}</>}
      width={220}
    >
      {(close) => (
        <Choices
          items={options}
          selected={(s) => s.id === status.id}
          filterText={(s) => s.name}
          render={(s) => (
            <>
              <Lozenge name={s.name} category={s.category} />
              {s.id === status.id && <Check size={14} style={{ marginInlineStart: 'auto' }} />}
            </>
          )}
          onPick={(s) => {
            close()
            if (s.id !== status.id) onChange(s.id)
          }}
        />
      )}
    </PopButton>
  )
}

export function PersonLabel({ person, size = 22, empty }: { person: Named | null | undefined; size?: number; empty?: string }) {
  return person ? (
    <>
      <Avatar name={person.name} id={person.id} size={size} decorative />
      <span style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>{person.name}</span>
    </>
  ) : (
    <>
      <Unassigned size={size} decorative />
      <span className="placeholder">{empty ?? t('Unassigned')}</span>
    </>
  )
}

export function PersonPicker({ value, onChange, disabled, allowNone = true, label, className, compact }: { value: Named | null; onChange: (id: string | null) => void; disabled?: boolean; allowNone?: boolean; label?: string; className?: string; compact?: boolean }) {
  const people = usePeople()
  const me = useMe().data?.user
  const items: (Named | null)[] = [...(allowNone ? [null] : []), ...(people.data ?? [])]
  if (me && people.data) {
    const mine = people.data.find((p) => p.id === me.id)
    if (mine) items.splice(allowNone ? 1 : 0, 0, { id: mine.id, name: `${mine.name} (${t('me')})` })
  }
  return (
    <PopButton
      disabled={disabled}
      className={className ?? 'value-btn'}
      title={label ?? t('Assignee')}
      width={260}
      label={compact ? (value ? <Avatar name={value.name} id={value.id} size={22} /> : <Unassigned size={22} />) : <PersonLabel person={value} />}
    >
      {(close) => (
        <Choices
          items={items}
          placeholder={t('Search people…')}
          selected={(p) => (p?.id ?? null) === (value?.id ?? null)}
          filterText={(p) => p?.name ?? t('Unassigned')}
          render={(p) => <PersonLabel person={p} />}
          onPick={(p) => {
            close()
            onChange(p?.id ?? null)
          }}
        />
      )}
    </PopButton>
  )
}

export function PriorityPicker({ value, onChange, disabled, compact }: { value: Priority; onChange: (p: Priority) => void; disabled?: boolean; compact?: boolean }) {
  return (
    <PopButton disabled={disabled} className={compact ? 'cell-btn' : 'value-btn'} title={t('Priority')} width={200} label={<PriorityIcon priority={value} withLabel={!compact} />}>
      {(close) => (
        <Choices
          items={PRIORITIES}
          selected={(p) => p === value}
          filterText={(p) => priorityLabel(p)}
          render={(p) => <PriorityIcon priority={p} withLabel />}
          onPick={(p) => {
            close()
            onChange(p)
          }}
        />
      )}
    </PopButton>
  )
}

export function TypePicker({ project, value, onChange, disabled, levels }: { project: Project; value: Issue['type']; onChange: (id: string) => void; disabled?: boolean; levels?: number[] }) {
  const types = project.types.filter((x) => !levels || levels.includes(x.level))
  return (
    <PopButton
      disabled={disabled}
      className="value-btn"
      title={t('Type')}
      width={220}
      label={
        <>
          <TypeIcon icon={value.icon} color={value.color} />
          {value.name}
        </>
      }
    >
      {(close) => (
        <Choices
          items={types}
          selected={(x) => x.id === value.id}
          filterText={(x) => x.name}
          render={(x) => (
            <>
              <TypeIcon icon={x.icon} color={x.color} />
              {x.name}
            </>
          )}
          onPick={(x) => {
            close()
            onChange(x.id)
          }}
        />
      )}
    </PopButton>
  )
}

function SimplePicker<T extends { id: string; name: string }>({ items, value, onChange, disabled, title, none, icon }: { items: T[]; value: { id: string; name: string } | null; onChange: (id: string | null) => void; disabled?: boolean; title: string; none: string; icon?: (x: T) => ReactNode }) {
  return (
    <PopButton disabled={disabled} className="value-btn" title={title} width={240} label={value ? <span>{value.name}</span> : <span className="placeholder">{none}</span>}>
      {(close) => (
        <Choices
          items={[null, ...items] as (T | null)[]}
          selected={(x) => (x?.id ?? null) === (value?.id ?? null)}
          filterText={(x) => x?.name ?? none}
          render={(x) => (x ? <>{icon?.(x)}{x.name}</> : <span className="faint">{none}</span>)}
          onPick={(x) => {
            close()
            onChange(x?.id ?? null)
          }}
        />
      )}
    </PopButton>
  )
}

export function SprintPicker({ project, value, onChange, disabled }: { project: Project; value: Issue['sprint']; onChange: (id: string | null) => void; disabled?: boolean }) {
  const items = project.sprints.map((s) => ({ id: s.id, name: s.state === 'active' ? `${s.name} · ${t('active')}` : s.name }))
  return <SimplePicker items={items} value={value} onChange={onChange} disabled={disabled} title={t('Sprint')} none={t('Backlog')} />
}

export function VersionPicker({ project, value, onChange, disabled }: { project: Project; value: Named | null; onChange: (id: string | null) => void; disabled?: boolean }) {
  const items = project.versions.filter((v) => v.status !== 'archived').map((v) => ({ id: v.id, name: v.status === 'released' ? `${v.name} · ${t('released')}` : v.name }))
  return <SimplePicker items={items} value={value} onChange={onChange} disabled={disabled} title={t('Release')} none={t('None')} />
}

export function ComponentPicker({ project, value, onChange, disabled }: { project: Project; value: Named | null; onChange: (id: string | null) => void; disabled?: boolean }) {
  return <SimplePicker items={project.components} value={value} onChange={onChange} disabled={disabled} title={t('Component')} none={t('None')} />
}

export function LabelsEditor({ value, onChange, disabled }: { value: string[]; onChange: (l: string[]) => void; disabled?: boolean }) {
  const [text, setText] = useState('')
  const known = useQuery({ queryKey: ['labels', text], queryFn: () => api.get<{ label: string; count: number }[]>(`/labels?prefix=${encodeURIComponent(text)}`), staleTime: 30_000 })
  const add = (l: string) => {
    const n = l.trim().toLowerCase().replace(/\s+/g, '-')
    if (n && !value.includes(n)) onChange([...value, n])
    setText('')
  }
  return (
    <PopButton
      disabled={disabled}
      className="value-btn"
      title={t('Labels')}
      width={260}
      label={value.length ? <span className="row-wrap" style={{ gap: 4 }}>{value.map((l) => <span key={l} className="label-chip">{l}</span>)}</span> : <span className="placeholder">{t('None')}</span>}
    >
      {() => (
        <div style={{ padding: 4 }}>
          <div className="row-wrap" style={{ gap: 4, marginBottom: 6 }}>
            {value.map((l) => (
              <span key={l} className="label-chip">
                {l}
                <button className="icon-btn icon-btn-sm" style={{ width: 16, height: 16 }} aria-label={t('Remove {label}', { label: l })} onClick={() => onChange(value.filter((x) => x !== l))}>
                  <X size={11} />
                </button>
              </span>
            ))}
          </div>
          <input
            className="input input-sm"
            autoFocus
            placeholder={t('Add a label…')}
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ',') {
                e.preventDefault()
                add(text)
              }
            }}
          />
          <div className="choices-list" style={{ marginTop: 4 }}>
            {(known.data ?? []).filter((k) => !value.includes(k.label)).slice(0, 8).map((k) => (
              <button key={k.label} className="menu-item" onClick={() => add(k.label)}>
                <span className="label-chip">{k.label}</span>
                <span className="mi-desc">{k.count}</span>
              </button>
            ))}
            {text.trim() && !(known.data ?? []).some((k) => k.label === text.trim().toLowerCase()) && (
              <button className="menu-item" onClick={() => add(text)}>
                <Plus size={14} /> {t('Create “{label}”', { label: text.trim() })}
              </button>
            )}
          </div>
        </div>
      )}
    </PopButton>
  )
}

export function DateValue({ value, onChange, disabled, highlightOverdue }: { value: string | null; onChange: (d: string | null) => void; disabled?: boolean; highlightOverdue?: boolean }) {
  const overdue = highlightOverdue && value && new Date(value + 'T23:59:59') < new Date()
  return (
    <span className="row" style={{ gap: 4 }}>
      <input
        type="date"
        className="input input-sm"
        style={{ width: 150, color: overdue ? 'var(--danger)' : undefined }}
        disabled={disabled}
        value={value ?? ''}
        aria-label={t('Date')}
        onChange={(e) => onChange(e.target.value || null)}
      />
      {value && !disabled && (
        <button className="icon-btn icon-btn-sm" aria-label={t('Clear')} onClick={() => onChange(null)}>
          <X size={13} />
        </button>
      )}
    </span>
  )
}

/** A number edited in place (story points, hours). */
export function NumberValue({ value, onChange, disabled, suffix, placeholder }: { value: number | null; onChange: (n: number | null) => void; disabled?: boolean; suffix?: string; placeholder?: string }) {
  const [text, setText] = useState(value?.toString() ?? '')
  useEffect(() => setText(value?.toString() ?? ''), [value])
  const commit = () => {
    const n = text.trim() === '' ? null : Number(text.replace(',', '.'))
    if (n !== null && (Number.isNaN(n) || n < 0)) {
      setText(value?.toString() ?? '')
      return
    }
    if (n !== value) onChange(n)
  }
  return (
    <span className="row" style={{ gap: 6 }}>
      <input
        className="input input-sm"
        style={{ width: 80 }}
        inputMode="decimal"
        disabled={disabled}
        value={text}
        placeholder={placeholder ?? '–'}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => e.key === 'Enter' && (e.target as HTMLInputElement).blur()}
      />
      {suffix && <span className="faint small">{suffix}</span>}
    </span>
  )
}

export function ParentPicker({ value, onChange, disabled, epicsOnly, projectKey }: { value: Issue['parent']; onChange: (id: string | null) => void; disabled?: boolean; epicsOnly?: boolean; projectKey: string }) {
  const [q, setQ] = useState('')
  const found = useQuery({
    queryKey: ['parent-search', projectKey, q, epicsOnly],
    queryFn: async () => {
      if (epicsOnly) {
        const r = await api.get<{ issues: Issue[] }>(`/search?project=${encodeURIComponent(projectKey)}&limit=50&q=${encodeURIComponent(`type in (${'Epic'}, Milestone) and category != done${q ? ` and text ~ "${q.replace(/"/g, '')}"` : ''}`)}`)
        if (r.issues.length) return r.issues
      }
      return api.get<Issue[]>(`/quick?project=${encodeURIComponent(projectKey)}&q=${encodeURIComponent(q)}`)
    },
  })
  return (
    <PopButton
      disabled={disabled}
      className="value-btn"
      title={t('Parent')}
      width={320}
      label={
        value ? (
          <>
            <span className="dot" style={{ background: value.color ?? 'var(--accent)' }} />
            <span className="issue-key">{value.key}</span>
            <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{value.title}</span>
          </>
        ) : (
          <span className="placeholder">{t('None')}</span>
        )
      }
    >
      {(close) => (
        <div>
          <input className="input input-sm choices-search" autoFocus placeholder={t('Search issues…')} value={q} onChange={(e) => setQ(e.target.value)} />
          <div className="choices-list">
            {value && (
              <button className="menu-item" onClick={() => { close(); onChange(null) }}>
                <X size={14} /> {t('Remove parent')}
              </button>
            )}
            {(found.data ?? []).map((i) => (
              <button key={i.id} className="menu-item" onClick={() => { close(); onChange(i.id) }}>
                <TypeIcon icon={i.type.icon} color={i.type.color} />
                <span className="issue-key">{i.key}</span>
                <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{i.title}</span>
              </button>
            ))}
          </div>
        </div>
      )}
    </PopButton>
  )
}

export function CustomFieldValue({ field, value, onChange, disabled }: { field: CustomField; value: any; onChange: (v: any) => void; disabled?: boolean }) {
  const people = usePeople()
  switch (field.kind) {
    case 'number':
      return <NumberValue value={typeof value === 'number' ? value : null} onChange={onChange} disabled={disabled} />
    case 'date':
      return <DateValue value={value ?? null} onChange={onChange} disabled={disabled} />
    case 'checkbox':
      return <input type="checkbox" checked={!!value} disabled={disabled} onChange={(e) => onChange(e.target.checked)} aria-label={field.name} />
    case 'user': {
      const p = people.data?.find((x) => x.id === value) ?? null
      return <PersonPicker value={p} onChange={onChange} disabled={disabled} label={field.name} />
    }
    case 'select': {
      const opt = field.options.find((o) => o.id === value)
      return (
        <PopButton disabled={disabled} className="value-btn" title={field.name} width={220} label={opt ? <OptionChip label={opt.label} color={opt.color} /> : <span className="placeholder">{t('None')}</span>}>
          {(close) => (
            <Choices
              items={[null, ...field.options]}
              selected={(o) => (o?.id ?? null) === (value ?? null)}
              filterText={(o) => o?.label ?? ''}
              render={(o) => (o ? <OptionChip label={o.label} color={o.color} /> : <span className="faint">{t('None')}</span>)}
              onPick={(o) => {
                close()
                onChange(o?.id ?? null)
              }}
            />
          )}
        </PopButton>
      )
    }
    case 'multiselect': {
      const sel: string[] = Array.isArray(value) ? value : []
      return (
        <PopButton
          disabled={disabled}
          className="value-btn"
          title={field.name}
          width={220}
          label={sel.length ? <span className="row-wrap" style={{ gap: 4 }}>{field.options.filter((o) => sel.includes(o.id)).map((o) => <OptionChip key={o.id} label={o.label} color={o.color} />)}</span> : <span className="placeholder">{t('None')}</span>}
        >
          {() => (
            <div className="choices-list">
              {field.options.map((o) => (
                <button key={o.id} className={`menu-item ${sel.includes(o.id) ? 'selected' : ''}`} onClick={() => onChange(sel.includes(o.id) ? sel.filter((x) => x !== o.id) : [...sel, o.id])}>
                  <OptionChip label={o.label} color={o.color} />
                  {sel.includes(o.id) && <Check size={14} style={{ marginInlineStart: 'auto' }} />}
                </button>
              ))}
            </div>
          )}
        </PopButton>
      )
    }
    default:
      return <TextValue value={value ?? ''} onChange={(v) => onChange(v || null)} disabled={disabled} url={field.kind === 'url'} />
  }
}

export function OptionChip({ label, color }: { label: string; color?: string | null }) {
  return (
    <span className="label-chip" style={color ? { background: `color-mix(in srgb, ${color} 18%, transparent)`, color: 'var(--text)' } : undefined}>
      {color && <span className="dot" style={{ background: color }} />}
      {label}
    </span>
  )
}

function TextValue({ value, onChange, disabled, url }: { value: string; onChange: (v: string) => void; disabled?: boolean; url?: boolean }) {
  const [text, setText] = useState(value)
  useEffect(() => setText(value), [value])
  if (disabled && url && value)
    return (
      <a href={value} target="_blank" rel="noreferrer noopener">
        {value}
      </a>
    )
  return (
    <input
      className="input input-sm"
      disabled={disabled}
      value={text}
      placeholder={url ? 'https://…' : '–'}
      onChange={(e) => setText(e.target.value)}
      onBlur={() => text !== value && onChange(text)}
      onKeyDown={(e) => e.key === 'Enter' && (e.target as HTMLInputElement).blur()}
    />
  )
}

/** A due date as text with overdue / soon colouring. */
export function DueText({ date, done }: { date: string; done?: boolean }) {
  const d = new Date(date + 'T23:59:59')
  const days = (d.getTime() - Date.now()) / 86400000
  const cls = done ? '' : days < 0 ? 'overdue' : days < 2 ? 'soon' : ''
  return <span className={`ic-due ${cls}`}>{shortDate(date)}</span>
}
