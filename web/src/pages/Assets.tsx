// The asset inventory: the devices, services and other things issues are about. /assets lists
// what the caller sees (by set, type, status, owner, text); /assets/<tag> is one asset with its
// fields, what it depends on and what depends on it, its issues and its history.
import { t } from '../i18n'
import { useMemo, useState } from 'react'
import { Link, useNavigate, useParams, useSearchParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Boxes, Download, Plus, Settings2, Trash } from 'lucide-react'
import { api, ago, dateTime, qs, type Issue, type Named } from '../api'
import { Empty, ErrorBox, Loading, Modal, useMe, usePeople, useToast } from '../ui'
import { IssueLine } from './Home'
import { SetSettings } from './AssetSettings'
import './Assets.css'

export type AssetStatus = 'in_use' | 'in_stock' | 'maintenance' | 'retired'
export const ASSET_STATUSES: AssetStatus[] = ['in_use', 'in_stock', 'maintenance', 'retired']

export interface AssetField {
  id: string
  name: string
  kind: 'text' | 'number' | 'date' | 'select' | 'checkbox' | 'url' | 'user' | 'asset'
  options?: { id: string; label: string }[]
  typeId?: string
  required?: boolean
}

export interface AssetType {
  id: string
  name: string
  icon: string | null
  color: string | null
  tagPrefix: string
  fields: AssetField[]
  assetCount?: number
}

export interface AssetSet {
  id: string
  name: string
  description: string | null
  defaultRole: 'none' | 'viewer' | 'editor'
  assetCount: number
  role: 'viewer' | 'editor' | 'admin'
  types: AssetType[]
  categories: { id: string; parentId: string | null; name: string }[]
  members: { subjectType: 'user' | 'team'; subjectId: string; role: 'viewer' | 'editor' | 'admin'; name: string | null }[] | null
}

export interface Asset {
  id: string
  setId: string
  tag: string
  name: string
  status: AssetStatus
  type: { id: string; name: string; icon: string | null; color: string | null }
  category: Named | null
  owner: { kind: 'user' | 'team' | 'email'; id?: string; name: string | null } | null
  updatedAt?: string
}

interface AssetDetail extends Asset {
  fields: (AssetField & { value: unknown; label: string | null })[]
  role: 'viewer' | 'editor' | 'admin'
  relations: { id: string; kind: 'depends_on' | 'installed_in'; outward: boolean; asset: { id: string; tag: string; name: string; status: AssetStatus } }[]
  issues: Issue[]
  history: { at: string; actor: string | null; kind: string; data: Record<string, any> }[]
}

interface Graph {
  root: string
  nodes: Asset[]
  edges: { source: string; target: string; kind: string }[]
  affected: string[]
}

export function useAssetSets(enabled = true) {
  return useQuery({ queryKey: ['asset-sets'], queryFn: () => api.get<{ canCreate: boolean; sets: AssetSet[] }>('/asset-sets'), enabled, staleTime: 30_000 })
}

export function statusLabel(s: AssetStatus): string {
  return { in_use: t('In use'), in_stock: t('In stock'), maintenance: t('In maintenance'), retired: t('Retired') }[s] ?? s
}

export function StatusBadge({ status }: { status: AssetStatus }) {
  const cls = status === 'in_use' ? 'badge-accent' : status === 'maintenance' ? 'badge-warning' : ''
  return <span className={`badge ${cls}`}>{statusLabel(status)}</span>
}

const ownerName = (a: Asset) => a.owner?.name ?? ''

export default function Assets() {
  const { tag } = useParams()
  return tag ? <AssetPage tag={tag} /> : <AssetList />
}

// ---- the list -----------------------------------------------------------------------------------------

function AssetList() {
  const me = useMe().data
  const sets = useAssetSets()
  const [params, setParams] = useSearchParams()
  const get = (k: string) => params.get(k) ?? ''
  const set = (patch: Record<string, string | null>) => {
    const next = new URLSearchParams(params)
    for (const [k, v] of Object.entries(patch)) v ? next.set(k, v) : next.delete(k)
    setParams(next, { replace: true })
  }
  const filters = { set: get('set') || null, type: get('type') || null, status: get('status') || null, owner: get('mine') ? 'me' : null, q: get('q') || null }
  const list = useQuery({ queryKey: ['assets', filters], queryFn: () => api.get<{ assets: Asset[]; more: boolean }>(`/assets${qs(filters)}`) })
  const [adding, setAdding] = useState(false)
  const [newSet, setNewSet] = useState(false)
  const [settings, setSettings] = useState<string | null>(null)
  const all = sets.data?.sets ?? []
  const current = all.find((s) => s.id === get('set'))
  const types = (current ? [current] : all).flatMap((s) => s.types)
  const canAdd = all.some((s) => s.role !== 'viewer' && s.types.length > 0)
  if (sets.isLoading) return <Loading />
  if (sets.error) return <div className="page"><ErrorBox error={sets.error} /></div>
  return (
    <div className="page">
      <div className="page-head">
        <h1><Boxes size={20} /> {t('Assets')}</h1>
        <span className="spacer" />
        {current && <a className="btn btn-sm" href={`/api/asset-sets/${current.id}/export.csv`}><Download size={14} /> {t('Export CSV')}</a>}
        {current?.role === 'admin' && <button className="btn btn-sm" onClick={() => setSettings(current.id)}><Settings2 size={14} /> {t('Set up {name}', { name: current.name })}</button>}
        {sets.data?.canCreate && <button className="btn btn-sm" onClick={() => setNewSet(true)}><Plus size={14} /> {t('New set')}</button>}
        {canAdd && <button className="btn btn-primary" onClick={() => setAdding(true)}><Plus size={15} /> {t('Add an asset')}</button>}
      </div>
      {all.length === 0 ? (
        <Empty icon={<Boxes size={32} />} title={t('No asset sets yet')}>
          {me?.user?.role === 'owner' || me?.user?.role === 'admin' ? t('A set — Hardware, Services, Locations — holds assets of its types and says who may see and change them. Start with “New set”.') : t('An admin sets up the inventory first.')}
        </Empty>
      ) : (
        <>
          <div className="tabs" role="tablist" aria-label={t('Asset sets')}>
            <button role="tab" aria-selected={!current} className={`tab ${!current ? 'active' : ''}`} onClick={() => set({ set: null, type: null })}>{t('All sets')}</button>
            {all.map((s) => (
              <button key={s.id} role="tab" aria-selected={current?.id === s.id} className={`tab ${current?.id === s.id ? 'active' : ''}`} onClick={() => set({ set: s.id, type: null })}>
                {s.name} <span className="badge">{s.assetCount}</span>
              </button>
            ))}
          </div>
          {current && current.types.length === 0 && current.role === 'admin' && (
            <p className="ps-note">{t('This set has no types yet (Laptop, Server, …): add one under “Set up”.')}</p>
          )}
          <div className="row-wrap as-filters">
            <input className="input input-sm" style={{ width: 240 }} type="search" placeholder={t('Search tag, name or a field…')} aria-label={t('Search assets')} defaultValue={get('q')} onChange={(e) => set({ q: e.target.value || null })} />
            <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Type')} value={get('type')} onChange={(e) => set({ type: e.target.value || null })}>
              <option value="">{t('All types')}</option>
              {types.map((ty) => <option key={ty.id} value={ty.id}>{ty.name}</option>)}
            </select>
            <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Status')} value={get('status')} onChange={(e) => set({ status: e.target.value || null })}>
              <option value="">{t('Any status')}</option>
              {ASSET_STATUSES.map((s) => <option key={s} value={s}>{statusLabel(s)}</option>)}
            </select>
            <label className="row small" style={{ gap: 6 }}><input type="checkbox" checked={!!get('mine')} onChange={(e) => set({ mine: e.target.checked ? '1' : null })} /> {t('Mine')}</label>
          </div>
          {list.isLoading && <Loading />}
          {list.error && <ErrorBox error={list.error} />}
          {list.data && list.data.assets.length === 0 && <Empty icon={<Boxes size={32} />} title={t('No assets match')} />}
          {list.data && list.data.assets.length > 0 && (
            <table className="list as-table">
              <thead>
                <tr><th>{t('Tag')}</th><th>{t('Name')}</th><th>{t('Type')}</th><th>{t('Status')}</th><th>{t('Category')}</th><th>{t('Owner')}</th></tr>
              </thead>
              <tbody>
                {list.data.assets.map((a) => (
                  <tr key={a.id}>
                    <td><Link to={`/assets/${a.tag}`} className="issue-key">{a.tag}</Link></td>
                    <td><Link to={`/assets/${a.tag}`}>{a.name}</Link></td>
                    <td className="small">{a.type.name}</td>
                    <td><StatusBadge status={a.status} /></td>
                    <td className="small faint">{a.category?.name}</td>
                    <td className="small">{ownerName(a)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {list.data?.more && <p className="faint small">{t('There are more: narrow the search.')}</p>}
        </>
      )}
      {adding && <AssetDialog sets={all} initialSet={current?.id} onClose={() => setAdding(false)} />}
      {newSet && <NewSetDialog onClose={() => setNewSet(false)} onCreated={(id) => { setNewSet(false); set({ set: id }); setSettings(id) }} />}
      {settings && all.find((s) => s.id === settings) && <SetSettings set={all.find((s) => s.id === settings)!} onClose={() => setSettings(null)} />}
    </div>
  )
}

function NewSetDialog({ onClose, onCreated }: { onClose: () => void; onCreated: (id: string) => void }) {
  const qc = useQueryClient()
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [role, setRole] = useState<'none' | 'viewer' | 'editor'>('viewer')
  const [err, setErr] = useState('')
  const save = async () => {
    try {
      const r = await api.post<{ id: string }>('/asset-sets', { name: name.trim(), description: description.trim() || undefined, defaultRole: role })
      await qc.invalidateQueries({ queryKey: ['asset-sets'] })
      onCreated(r.id)
    } catch (e) {
      setErr((e as Error).message)
    }
  }
  return (
    <Modal title={t('New asset set')} onClose={onClose} width={480} footer={<><button className="btn" onClick={onClose}>{t('Cancel')}</button><button className="btn btn-primary" disabled={!name.trim()} onClick={save}>{t('Create set')}</button></>}>
      <div className="field"><label htmlFor="as-set-name">{t('Name')}</label><input id="as-set-name" className="input" autoFocus maxLength={80} placeholder={t('e.g. Hardware')} value={name} onChange={(e) => setName(e.target.value)} /></div>
      <div className="field"><label htmlFor="as-set-desc">{t('Description')}</label><input id="as-set-desc" className="input" value={description} onChange={(e) => setDescription(e.target.value)} /></div>
      <DefaultRole id="as-set-role" value={role} onChange={setRole} />
      {err && <p className="error-text" role="alert">{err}</p>}
    </Modal>
  )
}

export function DefaultRole({ id, value, onChange }: { id: string; value: 'none' | 'viewer' | 'editor'; onChange: (r: 'none' | 'viewer' | 'editor') => void }) {
  return (
    <div className="field">
      <label htmlFor={id}>{t('Everyone in the workspace')}</label>
      <select id={id} className="select" value={value} onChange={(e) => onChange(e.target.value as 'none' | 'viewer' | 'editor')}>
        <option value="viewer">{t('sees the assets')}</option>
        <option value="editor">{t('sees and changes the assets')}</option>
        <option value="none">{t('sees nothing (only the people and teams given a role)')}</option>
      </select>
      <span className="field-hint">{t('Guests only see sets they’re given a role in.')}</span>
    </div>
  )
}

// ---- adding and editing an asset ----------------------------------------------------------------------

/** An input for one field of a type, by its kind. */
function FieldInput({ field, value, onChange }: { field: AssetField; value: unknown; onChange: (v: unknown) => void }) {
  const people = usePeople().data ?? []
  const id = `as-f-${field.id}`
  const label = <label htmlFor={id}>{field.name}{field.required && <span aria-hidden> *</span>}</label>
  switch (field.kind) {
    case 'checkbox':
      return <label className="row small as-check"><input id={id} type="checkbox" checked={value === true} onChange={(e) => onChange(e.target.checked)} /> {field.name}</label>
    case 'select':
      return (
        <div className="field">{label}
          <select id={id} className="select" value={String(value ?? '')} onChange={(e) => onChange(e.target.value || null)}>
            <option value="">—</option>
            {(field.options ?? []).map((o) => <option key={o.id} value={o.id}>{o.label}</option>)}
          </select>
        </div>
      )
    case 'user':
      return (
        <div className="field">{label}
          <select id={id} className="select" value={String(value ?? '')} onChange={(e) => onChange(e.target.value || null)}>
            <option value="">—</option>
            {people.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
          </select>
        </div>
      )
    case 'asset':
      return <div className="field">{label}<AssetRef id={id} typeId={field.typeId} value={value as string | null} onChange={onChange} /></div>
    default:
      return (
        <div className="field">{label}
          <input id={id} className="input" type={field.kind === 'date' ? 'date' : field.kind === 'number' ? 'number' : field.kind === 'url' ? 'url' : 'text'} step="any" value={String(value ?? '')} required={field.required}
            onChange={(e) => onChange(e.target.value === '' ? null : e.target.value)} />
        </div>
      )
  }
}

/** Another asset, picked from the ones of a type. */
function AssetRef({ id, typeId, value, onChange }: { id: string; typeId?: string; value: string | null; onChange: (v: unknown) => void }) {
  const q = useQuery({ queryKey: ['assets', { type: typeId ?? null, ref: true }], queryFn: () => api.get<{ assets: Asset[] }>(`/assets${qs({ type: typeId, limit: 500 })}`) })
  return (
    <select id={id} className="select" value={value ?? ''} onChange={(e) => onChange(e.target.value || null)}>
      <option value="">—</option>
      {(q.data?.assets ?? []).map((a) => <option key={a.id} value={a.id}>{a.tag} {a.name}</option>)}
    </select>
  )
}

function AssetDialog({ sets, initialSet, asset, onClose }: { sets: AssetSet[]; initialSet?: string; asset?: AssetDetail; onClose: () => void }) {
  const qc = useQueryClient()
  const nav = useNavigate()
  const people = usePeople().data ?? []
  const teams = useQuery({ queryKey: ['teams'], queryFn: () => api.get<Named[]>('/teams') }).data ?? []
  const editable = sets.filter((s) => s.role !== 'viewer' && s.types.length > 0)
  const [setId, setSetId] = useState(asset?.setId ?? (editable.find((s) => s.id === initialSet) ?? editable[0])?.id ?? '')
  const set = sets.find((s) => s.id === setId)
  const [typeId, setTypeId] = useState(asset?.type.id ?? set?.types[0]?.id ?? '')
  const type = set?.types.find((ty) => ty.id === typeId) ?? set?.types[0]
  const [name, setName] = useState(asset?.name ?? '')
  const [tag, setTag] = useState(asset?.tag ?? '')
  const [status, setStatus] = useState<AssetStatus>(asset?.status ?? 'in_use')
  const [category, setCategory] = useState(asset?.category?.id ?? '')
  const [owner, setOwner] = useState(asset?.owner ? (asset.owner.kind === 'email' ? 'e:' : `${asset.owner.kind === 'team' ? 't' : 'u'}:${asset.owner.id}`) : '')
  const [email, setEmail] = useState(asset?.owner?.kind === 'email' ? asset.owner.name ?? '' : '')
  const [values, setValues] = useState<Record<string, unknown>>(() => Object.fromEntries((asset?.fields ?? []).map((f) => [f.id, f.value])))
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const save = async () => {
    if (!type) return
    setBusy(true)
    setErr('')
    const body: Record<string, unknown> = {
      name: name.trim(),
      status,
      categoryId: category || null,
      ownerId: owner.startsWith('u:') ? owner.slice(2) : null,
      ownerTeamId: owner.startsWith('t:') ? owner.slice(2) : null,
      ownerEmail: owner === 'e:' ? email.trim() : null,
      values: Object.fromEntries(type.fields.map((f) => [f.id, values[f.id] ?? null])),
    }
    try {
      if (asset) {
        await api.patch(`/assets/${asset.id}`, body)
        qc.invalidateQueries({ queryKey: ['asset'] })
      } else {
        const r = await api.post<{ tag: string }>('/assets', { ...body, typeId: type.id, tag: tag.trim() || undefined })
        nav(`/assets/${r.tag}`)
      }
      qc.invalidateQueries({ queryKey: ['assets'] })
      qc.invalidateQueries({ queryKey: ['asset-sets'] })
      onClose()
    } catch (e) {
      setErr((e as Error).message)
      setBusy(false)
    }
  }
  return (
    <Modal
      title={asset ? t('Edit {name}', { name: asset.tag }) : t('Add an asset')}
      onClose={onClose}
      width={600}
      footer={<><button className="btn" onClick={onClose}>{t('Cancel')}</button><button className="btn btn-primary" disabled={busy || !name.trim() || !type} onClick={save}>{asset ? t('Save') : t('Add asset')}</button></>}
    >
      {!asset && (
        <div className="ap-grid">
          {editable.length > 1 && (
            <div className="field">
              <label htmlFor="as-set">{t('Set')}</label>
              <select id="as-set" className="select" value={setId} onChange={(e) => { setSetId(e.target.value); setTypeId(sets.find((s) => s.id === e.target.value)?.types[0]?.id ?? ''); setCategory('') }}>
                {editable.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
              </select>
            </div>
          )}
          <div className="field">
            <label htmlFor="as-type">{t('Type')}</label>
            <select id="as-type" className="select" value={type?.id ?? ''} onChange={(e) => setTypeId(e.target.value)}>
              {(set?.types ?? []).map((ty) => <option key={ty.id} value={ty.id}>{ty.name}</option>)}
            </select>
          </div>
        </div>
      )}
      <div className="ap-grid">
        <div className="field"><label htmlFor="as-name">{t('Name')}</label><input id="as-name" className="input" autoFocus maxLength={200} value={name} onChange={(e) => setName(e.target.value)} /></div>
        {!asset && (
          <div className="field">
            <label htmlFor="as-tag">{t('Asset tag')}</label>
            <input id="as-tag" className="input" maxLength={40} value={tag} placeholder={type ? t('the next one: {prefix}-…', { prefix: type.tagPrefix }) : ''} onChange={(e) => setTag(e.target.value)} />
          </div>
        )}
        <div className="field">
          <label htmlFor="as-status">{t('Status')}</label>
          <select id="as-status" className="select" value={status} onChange={(e) => setStatus(e.target.value as AssetStatus)}>
            {ASSET_STATUSES.map((s) => <option key={s} value={s}>{statusLabel(s)}</option>)}
          </select>
        </div>
        {(set?.categories.length ?? 0) > 0 && (
          <div className="field">
            <label htmlFor="as-cat">{t('Category')}</label>
            <select id="as-cat" className="select" value={category} onChange={(e) => setCategory(e.target.value)}>
              <option value="">—</option>
              {categoryOptions(set!.categories).map((c) => <option key={c.id} value={c.id}>{c.label}</option>)}
            </select>
          </div>
        )}
        <div className="field">
          <label htmlFor="as-owner">{t('Owner')}</label>
          <select id="as-owner" className="select" value={owner} onChange={(e) => setOwner(e.target.value)}>
            <option value="">{t('Nobody')}</option>
            <option value="e:">{t('Someone without an account (an e-mail address)')}</option>
            <optgroup label={t('Teams')}>{teams.map((x) => <option key={x.id} value={`t:${x.id}`}>{x.name}</option>)}</optgroup>
            <optgroup label={t('People')}>{people.map((p) => <option key={p.id} value={`u:${p.id}`}>{p.name}</option>)}</optgroup>
          </select>
        </div>
        {owner === 'e:' && <div className="field"><label htmlFor="as-owner-email">{t('Owner’s e-mail address')}</label><input id="as-owner-email" className="input" type="email" value={email} onChange={(e) => setEmail(e.target.value)} /></div>}
      </div>
      {type && type.fields.length > 0 && (
        <div className="ap-grid">
          {type.fields.map((f) => <FieldInput key={f.id} field={f} value={values[f.id]} onChange={(v) => setValues((all) => ({ ...all, [f.id]: v }))} />)}
        </div>
      )}
      {err && <p className="error-text" role="alert">{err}</p>}
    </Modal>
  )
}

/** Categories as a flat list with their path ("Office / Zurich"). */
export function categoryOptions(cats: AssetSet['categories']): { id: string; label: string }[] {
  const path = (c: AssetSet['categories'][number]): string => {
    const parent = cats.find((p) => p.id === c.parentId)
    return parent ? `${path(parent)} / ${c.name}` : c.name
  }
  return cats.map((c) => ({ id: c.id, label: path(c) })).sort((a, b) => a.label.localeCompare(b.label))
}

// ---- one asset ----------------------------------------------------------------------------------------

function fieldText(f: AssetDetail['fields'][number]): React.ReactNode {
  if (f.value == null || f.value === '') return <span className="faint">—</span>
  if (f.kind === 'checkbox') return f.value ? t('Yes') : t('No')
  if (f.kind === 'url') return <a href={String(f.value)} target="_blank" rel="noreferrer noopener">{String(f.value)}</a>
  if (f.kind === 'asset' && f.label) return <Link to={`/assets/${f.label.split(' ')[0]}`}>{f.label}</Link>
  return f.label ?? String(f.value)
}

function describe(h: AssetDetail['history'][number]): string {
  const d = h.data ?? {}
  switch (h.kind) {
    case 'created':
      return t('added it')
    case 'related':
      return d.kind === 'installed_in' ? t('noted that it’s installed in {tag}', { tag: d.tag }) : t('noted that it depends on {tag}', { tag: d.tag })
    case 'unrelated':
      return t('removed its relation to {tag}', { tag: d.tag })
    case 'issue':
      return d.key ? t('linked the issue {key}', { key: d.key }) : t('linked a request')
    default: {
      const parts: string[] = []
      if (d.name) parts.push(t('renamed it to {name}', { name: d.name }))
      if (d.status) parts.push(t('changed the status from {a} to {b}', { a: statusLabel(d.status.from), b: statusLabel(d.status.to) }))
      if (d.owner !== undefined) parts.push(d.owner ? t('gave it to {name}', { name: d.owner }) : t('left it without an owner'))
      if (d.category) parts.push(t('changed its category'))
      if (d.fields?.length) parts.push(t('changed {fields}', { fields: d.fields.join(', ') }))
      return parts.join(' · ') || t('changed it')
    }
  }
}

function AssetPage({ tag }: { tag: string }) {
  const qc = useQueryClient()
  const toast = useToast()
  const nav = useNavigate()
  const q = useQuery({ queryKey: ['asset', tag.toUpperCase()], queryFn: () => api.get<AssetDetail>(`/assets/${encodeURIComponent(tag)}`), retry: false })
  const sets = useAssetSets().data?.sets ?? []
  const [editing, setEditing] = useState(false)
  const [kind, setKind] = useState<'depends_on' | 'installed_in'>('depends_on')
  const [target, setTarget] = useState('')
  const a = q.data
  if (q.isLoading) return <Loading />
  if (q.error || !a) return <div className="page"><Empty icon={<Boxes size={32} />} title={t('There is no such asset (or you can’t see it)')} /></div>
  const can = a.role !== 'viewer'
  const refresh = () => qc.invalidateQueries({ queryKey: ['asset'] })
  const run = async (f: () => Promise<unknown>) => {
    try {
      await f()
    } catch (e) {
      toast((e as Error).message)
    }
    refresh()
  }
  const outward = a.relations.filter((r) => r.outward)
  const inward = a.relations.filter((r) => !r.outward)
  const relLabel = (r: AssetDetail['relations'][number]) =>
    r.outward ? (r.kind === 'installed_in' ? t('installed in') : t('depends on')) : r.kind === 'installed_in' ? t('holds') : t('is needed by')
  return (
    <div className="page">
      <div className="crumbs"><Link to="/assets">{t('Assets')}</Link> / <Link to={`/assets?set=${a.setId}`}>{sets.find((s) => s.id === a.setId)?.name ?? '…'}</Link></div>
      <div className="page-head">
        <h1><span className="issue-key">{a.tag}</span> {a.name}</h1>
        <StatusBadge status={a.status} />
        <span className="spacer" />
        {can && <button className="btn btn-sm" onClick={() => setEditing(true)}>{t('Edit')}</button>}
        {a.role === 'admin' && (
          <button className="btn btn-sm btn-danger" onClick={() => { if (window.confirm(t('Delete {tag}? Its history goes with it.', { tag: a.tag }))) void run(async () => { await api.del(`/assets/${a.id}`); nav('/assets') }) }}>
            <Trash size={14} /> {t('Delete')}
          </button>
        )}
      </div>
      <div className="as-grid">
        <section className="panel">
          <div className="panel-head"><h2>{t('Details')}</h2></div>
          <dl className="as-facts">
            <div><dt>{t('Type')}</dt><dd>{a.type.name}</dd></div>
            <div><dt>{t('Owner')}</dt><dd>{a.owner?.name ?? <span className="faint">—</span>}</dd></div>
            {a.category && <div><dt>{t('Category')}</dt><dd>{a.category.name}</dd></div>}
            {a.fields.map((f) => <div key={f.id}><dt>{f.name}</dt><dd>{fieldText(f)}</dd></div>)}
          </dl>
        </section>
        <section className="panel">
          <div className="panel-head"><h2>{t('Relations')}</h2></div>
          <div className="as-pad">
            {a.relations.length === 0 && <p className="faint small">{t('Nothing related yet: say what it depends on or where it’s installed, and you see what fails with it.')}</p>}
            <ul className="as-rels">
              {[...outward, ...inward].map((r) => (
                <li key={r.id}>
                  <span className="faint small">{relLabel(r)}</span>
                  <Link to={`/assets/${r.asset.tag}`}><span className="issue-key">{r.asset.tag}</span> {r.asset.name}</Link>
                  {r.asset.status !== 'in_use' && <StatusBadge status={r.asset.status} />}
                  <span className="spacer" />
                  {can && r.outward && <button className="icon-btn icon-btn-sm" aria-label={t('Remove the relation to {tag}', { tag: r.asset.tag })} onClick={() => run(() => api.del(`/asset-relations/${r.id}`))}><Trash size={13} /></button>}
                </li>
              ))}
            </ul>
            {can && (
              <form className="row-wrap" style={{ gap: 6 }} onSubmit={(e) => { e.preventDefault(); void run(async () => { await api.post(`/assets/${a.id}/relations`, { kind, target: target.trim() }); setTarget('') }) }}>
                <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Kind of relation')} value={kind} onChange={(e) => setKind(e.target.value as typeof kind)}>
                  <option value="depends_on">{t('depends on')}</option>
                  <option value="installed_in">{t('installed in')}</option>
                </select>
                <input className="input input-sm" style={{ width: 140 }} placeholder={t('a tag, e.g. SRV-0003')} aria-label={t('Tag of the other asset')} value={target} onChange={(e) => setTarget(e.target.value)} />
                <button className="btn btn-sm" disabled={!target.trim()}>{t('Relate')}</button>
              </form>
            )}
            {a.relations.length > 0 && <ImpactGraph id={a.id} />}
          </div>
        </section>
        <section className="panel">
          <div className="panel-head"><h2>{t('Issues')}</h2><span className="badge">{a.issues.length}</span><span className="spacer" /><Link className="small" to={`/search?q=${encodeURIComponent(`asset = "${a.tag}"`)}`}>{t('As a list')}</Link></div>
          {a.issues.length === 0 ? <p className="faint small as-pad">{t('No issues are about it. On an issue, add it under Assets.')}</p> : a.issues.map((i) => <IssueLine key={i.id} issue={i} />)}
        </section>
        <section className="panel">
          <div className="panel-head"><h2>{t('History')}</h2></div>
          <ul className="as-history">
            {a.history.map((h, n) => (
              <li key={n}><strong>{h.actor ?? h.data?.by ?? t('Automation')}</strong> {describe(h)} <span className="faint small" title={dateTime(h.at)}>· {ago(h.at)}</span></li>
            ))}
          </ul>
        </section>
      </div>
      {editing && <AssetDialog sets={sets} asset={a} onClose={() => setEditing(false)} />}
    </div>
  )
}

/** What the asset depends on (left) and what depends on it (right), three steps out. */
function ImpactGraph({ id }: { id: string }) {
  const q = useQuery({ queryKey: ['asset', 'graph', id], queryFn: () => api.get<Graph>(`/assets/${id}/graph`) })
  const layout = useMemo(() => {
    const g = q.data
    if (!g) return null
    // columns: 0 is the asset; negative what it needs, positive what needs it
    const col = new Map<string, number>([[g.root, 0]])
    for (let round = 0; round < 4; round++) {
      for (const e of g.edges) {
        if (col.has(e.source) && !col.has(e.target) && (col.get(e.source) ?? 0) <= 0) col.set(e.target, (col.get(e.source) ?? 0) - 1)
        if (col.has(e.target) && !col.has(e.source) && (col.get(e.target) ?? 0) >= 0) col.set(e.source, (col.get(e.target) ?? 0) + 1)
      }
    }
    const nodes = g.nodes.filter((n) => col.has(n.id))
    const cols = [...new Set(nodes.map((n) => col.get(n.id)!))].sort((a, b) => a - b)
    const pos = new Map<string, { x: number; y: number }>()
    let height = 0
    for (const c of cols) {
      const inCol = nodes.filter((n) => col.get(n.id) === c)
      inCol.forEach((n, i) => pos.set(n.id, { x: (c - cols[0]) * 190 + 8, y: i * 46 + 8 }))
      height = Math.max(height, inCol.length * 46 + 8)
    }
    return { g, nodes, pos, width: cols.length * 190, height }
  }, [q.data])
  if (!layout || layout.nodes.length < 2) return null
  const { g, nodes, pos, width, height } = layout
  return (
    <figure className="as-graph">
      <figcaption className="small faint">
        {g.affected.length > 0 ? t('If it fails, {count} others are affected (to its right).', { count: g.affected.length }) : t('Nothing depends on it.')}
      </figcaption>
      <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} role="group" aria-label={t('What it depends on and what depends on it')}>
        {g.edges.map((e, i) => {
          const s = pos.get(e.source)
          const tg = pos.get(e.target)
          if (!s || !tg) return null
          return <line key={i} x1={s.x} y1={s.y + 17} x2={tg.x + 170} y2={tg.y + 17} className={e.kind === 'installed_in' ? 'as-edge as-edge-in' : 'as-edge'} />
        })}
        {nodes.map((n) => {
          const p = pos.get(n.id)!
          return (
            <a key={n.id} href={`/assets/${n.tag}`}>
              <rect x={p.x} y={p.y} width={170} height={34} rx={7} className={`as-node${n.id === g.root ? ' as-root' : g.affected.includes(n.id) ? ' as-affected' : ''}`} />
              <text x={p.x + 8} y={p.y + 15} className="as-node-tag">{n.tag}</text>
              <text x={p.x + 8} y={p.y + 28} className="as-node-name">{n.name.length > 24 ? `${n.name.slice(0, 23)}…` : n.name}</text>
            </a>
          )
        })}
      </svg>
    </figure>
  )
}
