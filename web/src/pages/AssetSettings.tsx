// Setting up an asset set: its types with their fields, categories, who has which role, the
// rules that act on its assets, and importing assets from CSV.
import { t } from '../i18n'
import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus, Trash } from 'lucide-react'
import { api, ago, type Named } from '../api'
import { Modal, Tabs, usePeople, useProjects, useToast } from '../ui'
import { ASSET_STATUSES, DefaultRole, categoryOptions, statusLabel, type AssetField, type AssetSet, type AssetStatus, type AssetType } from './Assets'

type Tab = 'types' | 'categories' | 'people' | 'rules' | 'import'

const KINDS: { id: AssetField['kind']; label: () => string }[] = [
  { id: 'text', label: () => t('Text') },
  { id: 'number', label: () => t('Number') },
  { id: 'date', label: () => t('Date') },
  { id: 'select', label: () => t('Choice') },
  { id: 'checkbox', label: () => t('Checkbox') },
  { id: 'url', label: () => t('Web link') },
  { id: 'user', label: () => t('Person') },
  { id: 'asset', label: () => t('Another asset') },
]

export function SetSettings({ set, onClose }: { set: AssetSet; onClose: () => void }) {
  const [tab, setTab] = useState<Tab>('types')
  return (
    <Modal title={t('Set up {name}', { name: set.name })} onClose={onClose} width={820} footer={<button className="btn btn-primary" onClick={onClose}>{t('Done')}</button>}>
      <Tabs
        value={tab}
        onChange={setTab}
        tabs={[
          { id: 'types', label: t('Types') },
          { id: 'categories', label: t('Categories') },
          { id: 'people', label: t('Who has access') },
          { id: 'rules', label: t('Rules') },
          { id: 'import', label: t('Import') },
        ]}
      />
      <div style={{ marginTop: 12 }}>
        {tab === 'types' && <TypesTab set={set} />}
        {tab === 'categories' && <CategoriesTab set={set} />}
        {tab === 'people' && <PeopleTab set={set} />}
        {tab === 'rules' && <RulesTab set={set} />}
        {tab === 'import' && <ImportTab set={set} />}
      </div>
    </Modal>
  )
}

function useRun() {
  const qc = useQueryClient()
  const toast = useToast()
  return async (f: () => Promise<unknown>, done?: string) => {
    try {
      await f()
      if (done) toast(done)
    } catch (e) {
      toast((e as Error).message)
    }
    qc.invalidateQueries({ queryKey: ['asset-sets'] })
    qc.invalidateQueries({ queryKey: ['assets'] })
  }
}

// ---- types and their fields ---------------------------------------------------------------------------

function TypesTab({ set }: { set: AssetSet }) {
  const run = useRun()
  const [open, setOpen] = useState<string | null>(set.types[0]?.id ?? null)
  const [name, setName] = useState('')
  const [prefix, setPrefix] = useState('')
  return (
    <>
      <p className="muted small" style={{ marginTop: 0 }}>{t('A type says what an asset is — Laptop, Server, Room — and which fields it has. Its tag prefix numbers the assets: LT gives LT-0001.')}</p>
      {set.types.map((ty) => (
        <details key={ty.id} className="as-type" open={open === ty.id} onToggle={(e) => (e.currentTarget.open ? setOpen(ty.id) : open === ty.id && setOpen(null))}>
          <summary><strong>{ty.name}</strong> <span className="faint small">{ty.tagPrefix}-… · {t('{count} assets', { count: ty.assetCount ?? 0 })} · {t('{count} fields', { count: ty.fields.length })}</span></summary>
          {open === ty.id && <TypeEditor key={JSON.stringify(ty.fields)} set={set} type={ty} />}
        </details>
      ))}
      <form className="row-wrap" style={{ gap: 8, marginTop: 10 }} onSubmit={(e) => { e.preventDefault(); void run(async () => { await api.post(`/asset-sets/${set.id}/types`, { name: name.trim(), tagPrefix: prefix.trim() || undefined }); setName(''); setPrefix('') }) }}>
        <input className="input input-sm" style={{ width: 220 }} placeholder={t('New type, e.g. Laptop')} aria-label={t('Name of the new type')} value={name} onChange={(e) => setName(e.target.value)} />
        <input className="input input-sm" style={{ width: 110 }} placeholder={t('Prefix: LT')} aria-label={t('Tag prefix')} maxLength={8} value={prefix} onChange={(e) => setPrefix(e.target.value.toUpperCase())} />
        <button className="btn btn-sm btn-primary" disabled={!name.trim()}><Plus size={14} /> {t('Add type')}</button>
      </form>
    </>
  )
}

function TypeEditor({ set, type }: { set: AssetSet; type: AssetType }) {
  const run = useRun()
  const [fields, setFields] = useState<(AssetField & { optionsText?: string })[]>(type.fields.map((f) => ({ ...f, optionsText: (f.options ?? []).map((o) => o.label).join(', ') })))
  const allTypes = set.types
  const patch = (i: number, p: Partial<AssetField & { optionsText?: string }>) => setFields((all) => all.map((f, n) => (n === i ? { ...f, ...p } : f)))
  const save = () =>
    run(
      () =>
        api.patch(`/asset-types/${type.id}`, {
          fields: fields
            .filter((f) => f.name.trim())
            .map((f) => ({
              id: f.id || undefined,
              name: f.name.trim(),
              kind: f.kind,
              required: !!f.required,
              typeId: f.kind === 'asset' ? f.typeId || undefined : undefined,
              // keep the ids of options that are still there, so values stay
              options: f.kind === 'select' ? (f.optionsText ?? '').split(',').map((l) => l.trim()).filter(Boolean).map((label) => ({ label, id: f.options?.find((o) => o.label === label)?.id })) : undefined,
            })),
        }),
      t('Saved'),
    )
  return (
    <div className="as-type-body">
      {fields.map((f, i) => (
        <div key={i} className="row-wrap as-field-row">
          <input className="input input-sm" style={{ width: 180 }} aria-label={t('Field name')} placeholder={t('Field name')} value={f.name} onChange={(e) => patch(i, { name: e.target.value })} />
          <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Kind of field')} value={f.kind} disabled={!!f.id} onChange={(e) => patch(i, { kind: e.target.value as AssetField['kind'] })}>
            {KINDS.map((k) => <option key={k.id} value={k.id}>{k.label()}</option>)}
          </select>
          {f.kind === 'select' && <input className="input input-sm" style={{ flex: 1, minWidth: 160 }} aria-label={t('Options, separated by commas')} placeholder={t('Options, separated by commas')} value={f.optionsText ?? ''} onChange={(e) => patch(i, { optionsText: e.target.value })} />}
          {f.kind === 'asset' && (
            <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Assets of which type')} value={f.typeId ?? ''} onChange={(e) => patch(i, { typeId: e.target.value || undefined })}>
              <option value="">{t('Any asset')}</option>
              {allTypes.map((ty) => <option key={ty.id} value={ty.id}>{ty.name}</option>)}
            </select>
          )}
          <label className="row small" style={{ gap: 4 }}><input type="checkbox" checked={!!f.required} onChange={(e) => patch(i, { required: e.target.checked })} /> {t('Required')}</label>
          <button type="button" className="icon-btn icon-btn-sm" aria-label={t('Remove the field {name}', { name: f.name || String(i + 1) })} onClick={() => setFields((all) => all.filter((_, n) => n !== i))}><Trash size={13} /></button>
        </div>
      ))}
      <div className="row-wrap" style={{ gap: 8, marginTop: 6 }}>
        <button className="btn btn-sm" onClick={() => setFields((all) => [...all, { id: '', name: '', kind: 'text' }])}><Plus size={14} /> {t('Add a field')}</button>
        <button className="btn btn-sm btn-primary" onClick={save}>{t('Save fields')}</button>
        <span className="spacer" />
        <button className="btn btn-sm btn-danger" disabled={(type.assetCount ?? 0) > 0} title={(type.assetCount ?? 0) > 0 ? t('It still has assets') : undefined} onClick={() => run(() => api.del(`/asset-types/${type.id}`))}>{t('Delete type')}</button>
      </div>
      <p className="field-hint">{t('A field you remove takes its values with it when you save.')}</p>
    </div>
  )
}

// ---- categories ---------------------------------------------------------------------------------------

function CategoriesTab({ set }: { set: AssetSet }) {
  const run = useRun()
  const [name, setName] = useState('')
  const [parent, setParent] = useState('')
  const options = categoryOptions(set.categories)
  return (
    <>
      <p className="muted small" style={{ marginTop: 0 }}>{t('Categories sort assets into a tree — by site, department, purpose. Filtering by a category includes the ones below it.')}</p>
      <ul className="as-rels">
        {options.map((c) => (
          <li key={c.id}>
            <span>{c.label}</span>
            <span className="spacer" />
            <button className="icon-btn icon-btn-sm" aria-label={t('Delete {name}', { name: c.label })} onClick={() => run(() => api.del(`/asset-categories/${c.id}`))}><Trash size={13} /></button>
          </li>
        ))}
      </ul>
      <form className="row-wrap" style={{ gap: 8 }} onSubmit={(e) => { e.preventDefault(); void run(async () => { await api.post(`/asset-sets/${set.id}/categories`, { name: name.trim(), parentId: parent || null }); setName('') }) }}>
        <input className="input input-sm" style={{ width: 220 }} placeholder={t('New category')} aria-label={t('Name of the new category')} value={name} onChange={(e) => setName(e.target.value)} />
        <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Below')} value={parent} onChange={(e) => setParent(e.target.value)}>
          <option value="">{t('At the top')}</option>
          {options.map((c) => <option key={c.id} value={c.id}>{t('Below {name}', { name: c.label })}</option>)}
        </select>
        <button className="btn btn-sm btn-primary" disabled={!name.trim()}><Plus size={14} /> {t('Add category')}</button>
      </form>
    </>
  )
}

// ---- who has access -----------------------------------------------------------------------------------

function PeopleTab({ set }: { set: AssetSet }) {
  const run = useRun()
  const people = usePeople().data ?? []
  const teams = useQuery({ queryKey: ['teams'], queryFn: () => api.get<Named[]>('/teams') }).data ?? []
  const members = set.members ?? []
  const [who, setWho] = useState('')
  const [role, setRole] = useState<'viewer' | 'editor' | 'admin'>('editor')
  const save = (next: { subjectType: string; subjectId: string; role: string }[]) => run(() => api.patch(`/asset-sets/${set.id}`, { members: next }))
  const roleLabel = (r: string) => (r === 'admin' ? t('administers the set') : r === 'editor' ? t('changes assets') : t('sees assets'))
  return (
    <>
      <DefaultRole id="as-default-role" value={set.defaultRole} onChange={(r) => run(() => api.patch(`/asset-sets/${set.id}`, { defaultRole: r }))} />
      <p className="muted small">{t('Viewers see the set’s assets; editors add and change them; admins also set up types, rules and access. Workspace admins administer every set.')}</p>
      <ul className="as-rels">
        {members.map((m) => (
          <li key={m.subjectType + m.subjectId}>
            <span>{m.name ?? '…'} {m.subjectType === 'team' && <span className="badge">{t('Team')}</span>}</span>
            <span className="faint small">{roleLabel(m.role)}</span>
            <span className="spacer" />
            <button className="icon-btn icon-btn-sm" aria-label={t('Remove {name}', { name: m.name ?? '' })} onClick={() => save(members.filter((x) => x !== m))}><Trash size={13} /></button>
          </li>
        ))}
      </ul>
      <form className="row-wrap" style={{ gap: 8 }} onSubmit={(e) => { e.preventDefault(); const [subjectType, subjectId] = who.split(':'); void save([...members.filter((m) => m.subjectId !== subjectId), { subjectType, subjectId, role }]); setWho('') }}>
        <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Person or team')} value={who} onChange={(e) => setWho(e.target.value)}>
          <option value="">{t('Choose a person or team')}</option>
          <optgroup label={t('Teams')}>{teams.map((x) => <option key={x.id} value={`team:${x.id}`}>{x.name}</option>)}</optgroup>
          <optgroup label={t('People')}>{people.map((p) => <option key={p.id} value={`user:${p.id}`}>{p.name}</option>)}</optgroup>
        </select>
        <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Role')} value={role} onChange={(e) => setRole(e.target.value as typeof role)}>
          <option value="viewer">{roleLabel('viewer')}</option>
          <option value="editor">{roleLabel('editor')}</option>
          <option value="admin">{roleLabel('admin')}</option>
        </select>
        <button className="btn btn-sm btn-primary" disabled={!who}><Plus size={14} /> {t('Add')}</button>
      </form>
    </>
  )
}

// ---- rules --------------------------------------------------------------------------------------------

interface Rule {
  id: string
  name: string
  enabled: boolean
  trigger: { event: 'created' | 'changed' | 'date'; typeId?: string; status?: AssetStatus; fieldId?: string; days?: number }
  actions: { action: 'createIssue' | 'notifyOwner' | 'setStatus'; projectKey?: string; title?: string; assignOwner?: boolean; message?: string; status?: AssetStatus }[]
  runs: number
  lastRunAt: string | null
  lastError: string | null
}

function RulesTab({ set }: { set: AssetSet }) {
  const qc = useQueryClient()
  const toast = useToast()
  const projects = useProjects().data ?? []
  const q = useQuery({ queryKey: ['asset-rules', set.id], queryFn: () => api.get<Rule[]>(`/asset-sets/${set.id}/rules`) })
  const dateFields = set.types.flatMap((ty) => ty.fields.filter((f) => f.kind === 'date').map((f) => ({ id: f.id, label: `${ty.name}: ${f.name}`, typeId: ty.id })))
  const [name, setName] = useState('')
  const [event, setEvent] = useState<'date' | 'created' | 'changed'>(dateFields.length ? 'date' : 'created')
  const [field, setField] = useState(dateFields[0]?.id ?? '')
  const [days, setDays] = useState('30')
  const [status, setStatus] = useState('')
  const [action, setAction] = useState<'createIssue' | 'notifyOwner' | 'setStatus'>('createIssue')
  const [project, setProject] = useState('')
  const [title, setTitle] = useState('{tag} {name}: ')
  const [assign, setAssign] = useState(true)
  const [message, setMessage] = useState('')
  const [newStatus, setNewStatus] = useState<AssetStatus>('maintenance')
  const refresh = () => qc.invalidateQueries({ queryKey: ['asset-rules', set.id] })
  const run = async (f: () => Promise<unknown>) => {
    try {
      await f()
    } catch (e) {
      toast((e as Error).message)
    }
    refresh()
  }
  const describe = (r: Rule) => {
    const when = r.trigger.event === 'date'
      ? t('{days} days before “{field}”', { days: r.trigger.days ?? 0, field: dateFields.find((f) => f.id === r.trigger.fieldId)?.label ?? '?' })
      : r.trigger.event === 'created' ? t('when an asset is added') : r.trigger.status ? t('when an asset changes and is {status}', { status: statusLabel(r.trigger.status) }) : t('when an asset changes')
    const what = r.actions.map((a) => (a.action === 'createIssue' ? t('create an issue in {project}', { project: a.projectKey ?? '?' }) : a.action === 'notifyOwner' ? t('notify the owner') : t('set the status to {status}', { status: statusLabel(a.status ?? 'in_use') }))).join(', ')
    return `${when}: ${what}`
  }
  const add = () =>
    run(async () => {
      const trigger: Rule['trigger'] = event === 'date' ? { event, fieldId: field, days: Number(days), typeId: dateFields.find((f) => f.id === field)?.typeId } : { event, ...(status ? { status: status as AssetStatus } : {}) }
      const act: Rule['actions'][number] = action === 'createIssue' ? { action, projectKey: project, title: title.trim(), assignOwner: assign } : action === 'notifyOwner' ? { action, message: message.trim() } : { action, status: newStatus }
      await api.post(`/asset-sets/${set.id}/rules`, { name: name.trim(), trigger, actions: [act] })
      setName('')
    })
  return (
    <>
      <p className="muted small" style={{ marginTop: 0 }}>{t('Rules act on this set’s assets: when one is added or changes, or some days before one of its dates — “30 days before the warranty ends, create an issue for the owner”.')}</p>
      <ul className="as-rels">
        {(q.data ?? []).map((r) => (
          <li key={r.id} style={{ alignItems: 'flex-start' }}>
            <label className="ps-switch" title={r.enabled ? t('On') : t('Off')}>
              <input type="checkbox" checked={r.enabled} aria-label={t('Rule {name} is on', { name: r.name })} onChange={() => run(() => api.patch(`/asset-rules/${r.id}`, { enabled: !r.enabled }))} />
              <span aria-hidden />
            </label>
            <span style={{ flex: 1, minWidth: 0 }}>
              <strong>{r.name}</strong>
              <div className="small">{describe(r)}</div>
              <div className="faint small">{r.lastRunAt ? t('Ran {count} times, last {when}', { count: r.runs, when: ago(r.lastRunAt) }) : t('Hasn’t run yet')}</div>
              {r.lastError && <div className="error-text small">{t('Last run failed: {error}', { error: r.lastError })}</div>}
            </span>
            <button className="icon-btn icon-btn-sm" aria-label={t('Delete {name}', { name: r.name })} onClick={() => run(() => api.del(`/asset-rules/${r.id}`))}><Trash size={13} /></button>
          </li>
        ))}
      </ul>
      <fieldset className="ps-fieldset">
        <legend>{t('New rule')}</legend>
        <div className="row-wrap" style={{ gap: 8 }}>
          <input className="input input-sm" style={{ width: 200 }} placeholder={t('Name, e.g. Warranty')} aria-label={t('Name of the rule')} value={name} onChange={(e) => setName(e.target.value)} />
          <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('When')} value={event} onChange={(e) => setEvent(e.target.value as typeof event)}>
            {dateFields.length > 0 && <option value="date">{t('some days before a date')}</option>}
            <option value="created">{t('when an asset is added')}</option>
            <option value="changed">{t('when an asset changes')}</option>
          </select>
          {event === 'date' && (
            <>
              <input className="input input-sm" style={{ width: 80 }} type="number" min={0} max={3650} aria-label={t('Days before')} value={days} onChange={(e) => setDays(e.target.value)} />
              <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Date field')} value={field} onChange={(e) => setField(e.target.value)}>
                {dateFields.map((f) => <option key={f.id} value={f.id}>{f.label}</option>)}
              </select>
            </>
          )}
          {event === 'changed' && (
            <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Only when its status is')} value={status} onChange={(e) => setStatus(e.target.value)}>
              <option value="">{t('whatever its status')}</option>
              {ASSET_STATUSES.map((s) => <option key={s} value={s}>{t('and it is {status}', { status: statusLabel(s) })}</option>)}
            </select>
          )}
        </div>
        <div className="row-wrap" style={{ gap: 8, marginTop: 8 }}>
          <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Then')} value={action} onChange={(e) => setAction(e.target.value as typeof action)}>
            <option value="createIssue">{t('create an issue')}</option>
            <option value="notifyOwner">{t('notify the owner')}</option>
            <option value="setStatus">{t('set its status')}</option>
          </select>
          {action === 'createIssue' && (
            <>
              <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Project')} value={project} onChange={(e) => setProject(e.target.value)}>
                <option value="">{t('Choose a project')}</option>
                {projects.filter((p) => !p.archivedAt).map((p) => <option key={p.id} value={p.key}>{p.name}</option>)}
              </select>
              <input className="input input-sm" style={{ flex: 1, minWidth: 200 }} aria-label={t('Title of the issue')} value={title} onChange={(e) => setTitle(e.target.value)} />
              <label className="row small" style={{ gap: 4 }}><input type="checkbox" checked={assign} onChange={(e) => setAssign(e.target.checked)} /> {t('Assign it to the owner')}</label>
            </>
          )}
          {action === 'notifyOwner' && <input className="input input-sm" style={{ flex: 1, minWidth: 200 }} aria-label={t('Message')} placeholder={t('Message')} value={message} onChange={(e) => setMessage(e.target.value)} />}
          {action === 'setStatus' && (
            <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Status')} value={newStatus} onChange={(e) => setNewStatus(e.target.value as AssetStatus)}>
              {ASSET_STATUSES.map((s) => <option key={s} value={s}>{statusLabel(s)}</option>)}
            </select>
          )}
          <button className="btn btn-sm btn-primary" disabled={!name.trim() || (action === 'createIssue' && (!project || !title.trim())) || (event === 'date' && !field)} onClick={add}><Plus size={14} /> {t('Add rule')}</button>
        </div>
        <p className="field-hint">{t('Texts can use {tag}, {name}, {type}, {status}, {owner}, {date} and {field:Name}.', { tag: '{tag}', name: '{name}', type: '{type}', status: '{status}', owner: '{owner}', date: '{date}', 'field:Name': '{field:Name}' })}</p>
      </fieldset>
    </>
  )
}

// ---- CSV import ---------------------------------------------------------------------------------------

interface ImportResult {
  dryRun: boolean
  created: number
  updated: number
  errors: { row: number; message: string }[]
  columns: { header: string; target: string | null }[]
}

function ImportTab({ set }: { set: AssetSet }) {
  const qc = useQueryClient()
  const [typeId, setTypeId] = useState(set.types[0]?.id ?? '')
  const type = set.types.find((ty) => ty.id === typeId)
  const [csv, setCsv] = useState('')
  const [mapping, setMapping] = useState<Record<string, string>>({})
  const [result, setResult] = useState<ImportResult | null>(null)
  const [err, setErr] = useState('')
  const [busy, setBusy] = useState(false)
  const send = async (text: string, map: Record<string, string> | null, dryRun: boolean) => {
    setBusy(true)
    setErr('')
    try {
      const r = await api.post<ImportResult>(`/asset-types/${typeId}/import`, { csv: text, mapping: map ?? undefined, dryRun })
      setResult(r)
      if (!map) setMapping(Object.fromEntries(r.columns.map((c) => [c.header, c.target ?? ''])))
      if (!dryRun) {
        qc.invalidateQueries({ queryKey: ['assets'] })
        qc.invalidateQueries({ queryKey: ['asset-sets'] })
      }
    } catch (e) {
      setErr((e as Error).message)
      setResult(null)
    }
    setBusy(false)
  }
  const targets = [
    { id: '', label: t('— ignore —') },
    { id: 'name', label: t('Name') },
    { id: 'tag', label: t('Asset tag (updates the asset that has it)') },
    { id: 'status', label: t('Status') },
    { id: 'category', label: t('Category') },
    { id: 'owner', label: t('Owner (a person, a team or an address)') },
    ...(type?.fields ?? []).map((f) => ({ id: f.name, label: f.name })),
  ]
  if (set.types.length === 0) return <p className="faint">{t('Add a type first: a file holds assets of one type.')}</p>
  return (
    <>
      <p className="muted small" style={{ marginTop: 0 }}>{t('One asset per row, of one type. A row with the tag of an existing asset updates it; the others are added. Check first — nothing changes until you import.')}</p>
      <div className="row-wrap" style={{ gap: 8 }}>
        <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Type')} value={typeId} onChange={(e) => { setTypeId(e.target.value); setResult(null) }}>
          {set.types.map((ty) => <option key={ty.id} value={ty.id}>{ty.name}</option>)}
        </select>
        <label className="btn btn-sm">
          {t('Choose a CSV file…')}
          <input type="file" accept=".csv,text/csv" hidden onChange={async (e) => { const f = e.target.files?.[0]; if (f) { const text = await f.text(); setCsv(text); void send(text, null, true) } }} />
        </label>
      </div>
      {err && <p className="error-text" role="alert">{err}</p>}
      {result && (
        <>
          <table className="list" style={{ marginTop: 10 }}>
            <thead><tr><th>{t('Column in the file')}</th><th>{t('Goes to')}</th></tr></thead>
            <tbody>
              {result.columns.map((c) => (
                <tr key={c.header}>
                  <td>{c.header}</td>
                  <td>
                    <select className="select input-sm" aria-label={t('Where {column} goes', { column: c.header })} value={mapping[c.header] ?? ''} onChange={(e) => setMapping({ ...mapping, [c.header]: e.target.value })}>
                      {targets.map((x) => <option key={x.id} value={x.id}>{x.label}</option>)}
                    </select>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="small" role="status">
            {result.dryRun ? t('Checked: {created} would be added, {updated} updated.', { created: result.created, updated: result.updated }) : t('Imported: {created} added, {updated} updated.', { created: result.created, updated: result.updated })}
            {result.errors.length > 0 && <> {t('{count} rows have problems:', { count: result.errors.length })}</>}
          </p>
          {result.errors.length > 0 && <ul className="small error-text">{result.errors.slice(0, 20).map((e) => <li key={e.row}>{t('Row {row}: {message}', { row: e.row, message: e.message })}</li>)}</ul>}
          <div className="row-wrap" style={{ gap: 8 }}>
            <button className="btn btn-sm" disabled={busy} onClick={() => send(csv, mapping, true)}>{t('Check again')}</button>
            <button className="btn btn-sm btn-primary" disabled={busy || result.created + result.updated === 0} onClick={() => send(csv, mapping, false)}>{t('Import')}</button>
          </div>
        </>
      )}
    </>
  )
}
