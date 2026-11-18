import { t } from '../i18n'
import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { CalendarOff, Plus, Trash, Users } from 'lucide-react'
import { api, shortDate, type Absence, type Named } from '../api'
import { Confirm, Popover, useMe, usePeople, useToast } from '../ui'
import { PersonPicker } from './fields'

const KINDS = [
  { id: 'vacation', label: () => t('Vacation') },
  { id: 'sick', label: () => t('Sick') },
  { id: 'training', label: () => t('Training') },
  { id: 'other', label: () => t('Other') },
] as const

export function kindLabel(k: Absence['kind']) {
  return KINDS.find((x) => x.id === k)?.label() ?? t('Away')
}

export interface Holidays {
  regions: string[]
  myRegion: string
  holidays: { id: string; region: string; day: string; name: string }[]
}

export function useHolidays() {
  return useQuery({ queryKey: ['holidays'], queryFn: () => api.get<Holidays>('/holidays'), staleTime: 300_000 })
}

/** Absences in a period (everyone's; why only for one's own, or for admins). */
export function useAbsences(from?: string, to?: string, user?: string, project?: string) {
  const q = new URLSearchParams()
  if (from) q.set('from', from)
  if (to) q.set('to', to)
  if (user) q.set('user', user)
  if (project) q.set('project', project)
  return useQuery({ queryKey: ['absences', from ?? '', to ?? '', user ?? '', project ?? ''], queryFn: () => api.get<Absence[]>(`/absences?${q}`), staleTime: 60_000 })
}

function period(a: Absence) {
  const half = a.half === 'am' ? t('morning') : a.half === 'pm' ? t('afternoon') : null
  if (a.start === a.end) return half ? `${shortDate(a.start)} (${half})` : shortDate(a.start)
  return `${shortDate(a.start)} – ${shortDate(a.end)}`
}

/** Adds an absence: one's own, or (admins) someone else's. */
function AbsenceForm({ forOthers }: { forOthers?: boolean }) {
  const qc = useQueryClient()
  const toast = useToast()
  const people = usePeople().data ?? []
  const today = new Date().toISOString().slice(0, 10)
  const [who, setWho] = useState<Named | null>(null)
  const [start, setStart] = useState(today)
  const [end, setEnd] = useState(today)
  const [half, setHalf] = useState<'' | 'am' | 'pm'>('')
  const [kind, setKind] = useState<string>('vacation')
  const [note, setNote] = useState('')
  const [sub, setSub] = useState<Named | null>(null)
  const add = async () => {
    try {
      await api.post('/absences', { userId: forOthers ? who?.id : undefined, start, end: half ? start : end, half: half || null, kind, note, substituteId: sub?.id ?? null })
      setNote('')
      setHalf('')
      qc.invalidateQueries({ queryKey: ['absences'] })
      qc.invalidateQueries({ queryKey: ['people'] })
    } catch (e) {
      toast((e as Error).message)
    }
  }
  return (
    <div className="row-wrap" style={{ gap: 8, alignItems: 'flex-end', marginBottom: 12 }}>
      {forOthers && <PersonPicker className="btn" value={who} allowNone={false} label={t('Who')} none={t('Who?')} onChange={(id) => setWho(people.find((p) => p.id === id) ?? null)} />}
      <div className="field" style={{ marginBottom: 0 }}>
        <label htmlFor="ab-start">{t('From')}</label>
        <input id="ab-start" type="date" className="input" value={start} onChange={(e) => { setStart(e.target.value); if (e.target.value > end) setEnd(e.target.value) }} />
      </div>
      {!half && (
        <div className="field" style={{ marginBottom: 0 }}>
          <label htmlFor="ab-end">{t('To')}</label>
          <input id="ab-end" type="date" className="input" value={end} min={start} onChange={(e) => setEnd(e.target.value)} />
        </div>
      )}
      <select className="select" value={half} onChange={(e) => setHalf(e.target.value as typeof half)} aria-label={t('Whole or half day')}>
        <option value="">{t('Whole days')}</option>
        <option value="am">{t('Morning only')}</option>
        <option value="pm">{t('Afternoon only')}</option>
      </select>
      <select className="select" value={kind} onChange={(e) => setKind(e.target.value)} aria-label={t('Why (only you and admins see it)')}>
        {KINDS.map((k) => <option key={k.id} value={k.id}>{k.label()}</option>)}
      </select>
      <input className="input" style={{ width: 160 }} value={note} onChange={(e) => setNote(e.target.value)} placeholder={t('Note (private)')} aria-label={t('Note (private)')} />
      <PersonPicker className="btn" value={sub} label={t('Substitute')} none={t('No substitute')} onChange={(id) => setSub(id ? people.find((p) => p.id === id) ?? null : null)} />
      <button type="button" className="btn btn-primary" disabled={forOthers && !who} onClick={add}><Plus size={15} /> {t('Add absence')}</button>
    </div>
  )
}

function AbsenceTable({ list, showWho }: { list: Absence[]; showWho?: boolean }) {
  const qc = useQueryClient()
  const [deleting, setDeleting] = useState<Absence | null>(null)
  if (!list.length) return <p className="faint small st-none">{t('No absences.')}</p>
  return (
    <>
      <div className="st-table-wrap">
        <table className="list st-tokens">
          <thead>
            <tr>
              {showWho && <th>{t('Who')}</th>}
              <th>{t('Dates')}</th>
              <th>{t('Why')}</th>
              <th>{t('Substitute')}</th>
              <th><span className="sr-only">{t('Actions')}</span></th>
            </tr>
          </thead>
          <tbody>
            {list.map((a) => (
              <tr key={a.id}>
                {showWho && <td>{a.user.name}</td>}
                <td className="small">{period(a)}</td>
                <td className="small">{a.kind ? kindLabel(a.kind) : <span className="faint">{t('Away')}</span>}{a.note && <span className="faint"> · {a.note}</span>}</td>
                <td className="small">{a.substitute?.name ?? <span className="faint">—</span>}</td>
                <td className="st-tok-actions">
                  {a.editable && <button type="button" className="btn btn-sm btn-danger" onClick={() => setDeleting(a)}><Trash size={14} /> {t('Delete')}</button>}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {deleting && (
        <Confirm
          title={t('Delete this absence?')}
          body={<p>{period(deleting)}</p>}
          confirmLabel={t('Delete')}
          danger
          onConfirm={async () => {
            await api.del(`/absences/${deleting.id}`)
            qc.invalidateQueries({ queryKey: ['absences'] })
            qc.invalidateQueries({ queryKey: ['people'] })
          }}
          onClose={() => setDeleting(null)}
        />
      )}
    </>
  )
}

/** Profile & settings: one's own time off and whose public holidays apply. */
export function MyTimeOff() {
  const me = useMe().data?.user
  const qc = useQueryClient()
  const toast = useToast()
  const mine = useAbsences(undefined, undefined, me?.id)
  const hol = useHolidays().data
  return (
    <>
      <AbsenceForm />
      <AbsenceTable list={mine.data ?? []} />
      {hol && hol.regions.length > 1 && (
        <label className="row small" style={{ gap: 8, marginTop: 12 }}>
          {t('Public holidays of')}
          <select
            className="select input-sm"
            style={{ width: 'auto' }}
            value={hol.myRegion}
            onChange={async (e) => {
              try {
                await api.put('/me/holiday-region', { region: e.target.value })
              } catch (err) {
                toast((err as Error).message)
              }
              qc.invalidateQueries({ queryKey: ['holidays'] })
            }}
          >
            {hol.regions.map((r) => <option key={r} value={r}>{r || t('The workspace’s calendar')}</option>)}
          </select>
        </label>
      )}
    </>
  )
}

/** Administration → Time off: public holidays by region calendar, and everyone's absences. */
export function AdminTimeOff() {
  const qc = useQueryClient()
  const toast = useToast()
  const hol = useHolidays()
  const all = useAbsences()
  const [day, setDay] = useState('')
  const [name, setName] = useState('')
  const [region, setRegion] = useState('')
  const add = async () => {
    try {
      await api.post('/holidays', { day, name: name.trim(), region: region.trim() })
      setName('')
      qc.invalidateQueries({ queryKey: ['holidays'] })
    } catch (e) {
      toast((e as Error).message)
    }
  }
  return (
    <>
      <div className="adm-toolbar">
        <p className="small muted adm-intro">{t('Public holidays don’t count as working days when sprints are planned. Keep them in the workspace’s calendar, or in calendars per region (people choose theirs in their settings). People enter their own absences; you can enter them for others.')}</p>
      </div>
      <h3 className="adm-subhead"><CalendarOff size={15} /> {t('Public holidays')}</h3>
      <div className="row-wrap" style={{ gap: 8, marginBottom: 10 }}>
        <input type="date" className="input input-sm" value={day} onChange={(e) => setDay(e.target.value)} aria-label={t('Day')} />
        <input className="input input-sm" value={name} onChange={(e) => setName(e.target.value)} placeholder={t('Name, like New Year')} aria-label={t('Holiday name')} />
        <input className="input input-sm" list="hol-regions" value={region} onChange={(e) => setRegion(e.target.value)} placeholder={t('Region (empty: everyone)')} aria-label={t('Region calendar')} />
        <datalist id="hol-regions">{(hol.data?.regions ?? []).filter(Boolean).map((r) => <option key={r} value={r} />)}</datalist>
        <button type="button" className="btn btn-sm btn-primary" disabled={!day || !name.trim()} onClick={add}><Plus size={14} /> {t('Add holiday')}</button>
      </div>
      {(hol.data?.holidays.length ?? 0) > 0 && (
        <div className="adm-table-wrap">
          <table className="list adm-table">
            <thead>
              <tr><th>{t('Day')}</th><th>{t('Name')}</th><th>{t('Region')}</th><th><span className="sr-only">{t('Actions')}</span></th></tr>
            </thead>
            <tbody>
              {hol.data!.holidays.map((h) => (
                <tr key={h.id}>
                  <td className="small">{shortDate(h.day)}</td>
                  <td>{h.name}</td>
                  <td className="small">{h.region || <span className="faint">{t('Everyone')}</span>}</td>
                  <td className="adm-actions">
                    <button className="icon-btn" title={t('Delete')} aria-label={t('Delete {name}', { name: h.name })} onClick={async () => { await api.del(`/holidays/${h.id}`); qc.invalidateQueries({ queryKey: ['holidays'] }) }}><Trash size={15} /></button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <h3 className="adm-subhead" style={{ marginTop: 20 }}><Users size={15} /> {t('Absences')}</h3>
      <AbsenceForm forOthers />
      <AbsenceTable list={all.data ?? []} showWho />
    </>
  )
}

interface Capacity {
  start: string | null
  end: string | null
  people: { user: string; name: string; workingDays: number; holidays: number; away: number; available: number }[]
  workingDays: number
  available: number
  share: number | null
}

/** On a sprint with dates: the share of its working days the team is there, per person on click. */
export function SprintCapacity({ sprintId }: { sprintId: string }) {
  const [el, setEl] = useState<HTMLButtonElement | null>(null)
  const [open, setOpen] = useState(false)
  const cap = useQuery({ queryKey: ['capacity', sprintId], queryFn: () => api.get<Capacity>(`/sprints/${sprintId}/capacity`), staleTime: 60_000 })
  const c = cap.data
  if (!c || c.share == null || !c.people.length) return null
  const pct = Math.round(c.share * 100)
  const fmt = (n: number) => (Number.isInteger(n) ? String(n) : n.toFixed(1))
  return (
    <>
      <button ref={setEl} className={`badge${pct < 80 ? ' badge-warning' : ''}`} style={{ cursor: 'pointer', border: 0 }} onClick={() => setOpen((o) => !o)} title={t('Who is there in this sprint')}>
        {t('{pct}% there', { pct })}
      </button>
      {open && (
        <Popover anchor={el} onClose={() => setOpen(false)} width={340} className="capacity-pop">
          <div className="menu-head">{t('Days in the sprint')}</div>
          <table>
            <thead>
              <tr><th>{t('Who')}</th><th>{t('Working days')}</th><th>{t('Holidays')}</th><th>{t('Away')}</th><th>{t('There')}</th></tr>
            </thead>
            <tbody>
              {c.people.map((p) => (
                <tr key={p.user}>
                  <td>{p.name}</td>
                  <td>{fmt(p.workingDays)}</td>
                  <td>{fmt(p.holidays)}</td>
                  <td>{fmt(p.away)}</td>
                  <td><strong>{fmt(p.available)}</strong></td>
                </tr>
              ))}
            </tbody>
          </table>
          <p className="faint small" style={{ margin: '6px 8px' }}>{t('{available} of {total} working days, after public holidays and absences.', { available: fmt(c.available), total: fmt(c.workingDays) })}</p>
        </Popover>
      )}
    </>
  )
}
