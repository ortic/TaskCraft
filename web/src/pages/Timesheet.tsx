import { t, locale } from '../i18n'
import { useEffect, useState, type FormEvent, type KeyboardEvent } from 'react'
import { useSearchParams } from 'react-router-dom'
import { keepPreviousData, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronDown, ChevronLeft, ChevronRight, Plus, Timer, Trash } from 'lucide-react'
import { api, duration, isoDate, qs, type Issue } from '../api'
import { Avatar, Choices, ErrorBox, Loading, PopButton, Popover, Spinner, useMe, usePeople, useToast } from '../ui'
import { TypeIcon } from '../icons'
import { useOpenIssue } from '../components/Layout'
import './Timesheet.css'

interface SheetRow {
  id: string
  key: string
  title: string
  days: Record<string, number>
}

interface Sheet {
  from: string
  to: string
  days: number
  issues: SheetRow[]
}

interface Worklog {
  id: string
  user: { id: string; name: string } | null
  minutes: number
  date: string
  note: string | null
}

function parseDay(s: string | null): Date | null {
  if (!s || !/^\d{4}-\d{2}-\d{2}$/.test(s)) return null
  const d = new Date(s + 'T00:00:00')
  return Number.isNaN(d.getTime()) ? null : d
}

/** The first day (local midnight) of the week holding `d`; `first` is 0 for Sunday, 1 for Monday. */
function weekStart(d: Date, first: number): Date {
  const x = new Date(d)
  x.setHours(0, 0, 0, 0)
  x.setDate(x.getDate() - ((x.getDay() - first + 7) % 7))
  return x
}

function addDays(d: Date, n: number): Date {
  const x = new Date(d)
  x.setDate(x.getDate() + n)
  return x
}

function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value)
  useEffect(() => {
    const h = setTimeout(() => setV(value), ms)
    return () => clearTimeout(h)
  }, [value, ms])
  return v
}

export default function Timesheet() {
  const me = useMe().data
  const people = usePeople()
  const open = useOpenIssue()
  const [params, setParams] = useSearchParams()
  const [extra, setExtra] = useState<SheetRow[]>([])
  const [cell, setCell] = useState<{ row: SheetRow; date: string; anchor: HTMLElement } | null>(null)

  const myId = me?.user?.id
  const isAdmin = me?.user?.role === 'owner' || me?.user?.role === 'admin'
  const first = me?.workspace?.weekStart === 'sunday' ? 0 : 1
  const start = weekStart(parseDay(params.get('week')) ?? new Date(), first)
  const from = isoDate(start)
  const thisWeek = isoDate(weekStart(new Date(), first))
  const userParam = isAdmin ? params.get('user') : null
  const viewing = userParam && userParam !== myId ? userParam : null
  const editable = !viewing
  const viewed = viewing ? people.data?.find((p) => p.id === viewing) : null

  const sheet = useQuery({
    queryKey: ['timesheet', from, viewing ?? 'me'],
    queryFn: () => api.get<Sheet>(`/timesheet${qs({ from, days: 7, user: viewing })}`),
    placeholderData: keepPreviousData,
    enabled: !!me?.user,
  })

  const setParam = (k: string, v: string | null) =>
    setParams((p) => {
      const n = new URLSearchParams(p)
      if (v) n.set(k, v)
      else n.delete(k)
      return n
    })
  const goWeek = (d: Date) => {
    const iso = isoDate(weekStart(d, first))
    setParam('week', iso === thisWeek ? null : iso)
    setCell(null)
  }

  const days = Array.from({ length: 7 }, (_, i) => addDays(start, i))
  const isoDays = days.map(isoDate)
  const today = isoDate(new Date())
  const loaded = sheet.data?.issues ?? []
  const rows = [...loaded, ...(editable ? extra.filter((e) => !loaded.some((r) => r.id === e.id)) : [])]
  const rowTotal = (r: SheetRow) => isoDays.reduce((s, d) => s + (r.days[d] ?? 0), 0)
  const dayTotals = isoDays.map((d) => rows.reduce((s, r) => s + (r.days[d] ?? 0), 0))
  const grand = dayTotals.reduce((a, b) => a + b, 0)
  const weekday = new Intl.DateTimeFormat(locale(), { weekday: 'short' })
  const dayNum = new Intl.DateTimeFormat(locale(), { day: 'numeric', month: 'short' })
  const longDay = new Intl.DateTimeFormat(locale(), { weekday: 'long', day: 'numeric', month: 'long' })
  const rangeFmt = new Intl.DateTimeFormat(locale(), { day: 'numeric', month: 'short', year: 'numeric' })
  const range = rangeFmt.formatRange(start, days[6])

  if (!me?.user) return <Loading />
  return (
    <div className="page ts-page">
      <div className="page-head">
        <h1><Timer size={20} /> {viewing ? t('Timesheet of {name}', { name: viewed?.name ?? '…' }) : t('My timesheet')}</h1>
        <span className="spacer" />
        {isAdmin && (
          <PopButton
            className="btn ts-person"
            align="end"
            width={280}
            title={t('Show someone else’s timesheet')}
            label={
              <>
                <Avatar name={viewed?.name ?? me.user.name} id={viewing ?? myId} size={20} />
                <span className="ts-person-name">{viewed?.name ?? me.user.name}</span>
                <ChevronDown size={14} />
              </>
            }
          >
            {(close) => (
              <Choices
                items={people.data ?? []}
                filterText={(p) => `${p.name} ${p.email ?? ''}`}
                placeholder={t('Find people…')}
                selected={(p) => p.id === (viewing ?? myId)}
                onPick={(p) => {
                  close()
                  setCell(null)
                  setParam('user', p.id === myId ? null : p.id)
                }}
                render={(p) => (
                  <>
                    <Avatar name={p.name} id={p.id} size={22} />
                    <span className="ts-choice-name">{p.name}</span>
                    {p.id === myId && <span className="faint small">{t('you')}</span>}
                  </>
                )}
              />
            )}
          </PopButton>
        )}
      </div>

      <div className="ts-bar">
        <div className="ts-nav">
          <button className="icon-btn" onClick={() => goWeek(addDays(start, -7))} aria-label={t('Previous week')} title={t('Previous week')}><ChevronLeft size={18} className="ts-flip" /></button>
          <button className="btn btn-sm" onClick={() => goWeek(new Date())} disabled={from === thisWeek}>{t('This week')}</button>
          <button className="icon-btn" onClick={() => goWeek(addDays(start, 7))} aria-label={t('Next week')} title={t('Next week')}><ChevronRight size={18} className="ts-flip" /></button>
          <span className="ts-range">{range}</span>
          {sheet.isFetching && <Spinner size={14} />}
        </div>
        <span className="spacer" />
        <div className="ts-week-total">
          <span className="faint small">{t('Week total')}</span>
          <strong>{duration(grand)}</strong>
        </div>
      </div>

      {sheet.error && <ErrorBox error={sheet.error} />}
      {sheet.isLoading && <Loading />}
      {sheet.data && (
        <div className="ts-wrap">
          <table className="ts-grid">
            <thead>
              <tr>
                <th className="ts-issue-col" scope="col">{t('Issue')}</th>
                {days.map((d, i) => {
                  const iso = isoDays[i]
                  const weekend = d.getDay() === 0 || d.getDay() === 6
                  return (
                    <th key={iso} scope="col" className={`ts-day ${iso === today ? 'ts-today' : ''} ${weekend ? 'ts-weekend' : ''}`} aria-current={iso === today ? 'date' : undefined}>
                      <span className="ts-wd">{weekday.format(d)}</span>
                      <span className="ts-dn">{dayNum.format(d)}</span>
                    </th>
                  )
                })}
                <th className="ts-total-col" scope="col">{t('Total')}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => (
                <tr key={r.id}>
                  <th scope="row" className="ts-issue-col">
                    <button type="button" className="ts-issue" onClick={() => open(r.key)} title={`${r.key} ${r.title}`}>
                      <span className="issue-key">{r.key}</span>
                      <span className="ts-issue-title">{r.title}</span>
                    </button>
                  </th>
                  {isoDays.map((iso, i) => {
                    const m = r.days[iso] ?? 0
                    const weekend = days[i].getDay() === 0 || days[i].getDay() === 6
                    const active = cell?.row.id === r.id && cell.date === iso
                    return (
                      <td key={iso} className={`ts-cell-td ${iso === today ? 'ts-today' : ''} ${weekend ? 'ts-weekend' : ''}`}>
                        {editable ? (
                          <button
                            type="button"
                            className={`ts-cell ${m ? 'ts-has' : ''} ${active ? 'ts-active' : ''}`}
                            aria-label={m ? t('{key}, {day}: {time} logged. Log more time', { key: r.key, day: longDay.format(days[i]), time: duration(m) }) : t('Log time on {key} for {day}', { key: r.key, day: longDay.format(days[i]) })}
                            onClick={(e) => setCell({ row: r, date: iso, anchor: e.currentTarget })}
                          >
                            {m ? duration(m) : <Plus size={13} className="ts-plus" aria-hidden />}
                          </button>
                        ) : (
                          <span className={`ts-cell ts-ro ${m ? 'ts-has' : ''}`}>{m ? duration(m) : ''}</span>
                        )}
                      </td>
                    )
                  })}
                  <td className="ts-total-col">{duration(rowTotal(r))}</td>
                </tr>
              ))}
              {rows.length === 0 && (
                <tr>
                  <td colSpan={9} className="ts-empty">
                    {editable ? t('No time logged this week. Add an issue to log time on it.') : t('{name} logged no time this week.', { name: viewed?.name ?? t('This person') })}
                  </td>
                </tr>
              )}
              {editable && (
                <tr className="ts-add-row">
                  <td colSpan={9}>
                    <PopButton className="btn btn-sm btn-ghost ts-add" width={400} label={<><Plus size={14} /> {t('Add issue')}</>}>
                      {(close) => (
                        <IssueSearch
                          exclude={rows.map((r) => r.id)}
                          onPick={(i) => {
                            close()
                            setExtra((x) => [...x.filter((e) => e.id !== i.id), { id: i.id, key: i.key, title: i.title, days: {} }])
                          }}
                        />
                      )}
                    </PopButton>
                  </td>
                </tr>
              )}
            </tbody>
            <tfoot>
              <tr>
                <th scope="row" className="ts-issue-col">{t('Total')}</th>
                {dayTotals.map((m, i) => (
                  <td key={isoDays[i]} className={`${isoDays[i] === today ? 'ts-today' : ''} ${days[i].getDay() === 0 || days[i].getDay() === 6 ? 'ts-weekend' : ''}`}>
                    {m ? duration(m) : <span className="faint">–</span>}
                  </td>
                ))}
                <td className="ts-total-col ts-grand">{duration(grand)}</td>
              </tr>
            </tfoot>
          </table>
        </div>
      )}
      <p className="faint small ts-hint">
        {editable
          ? t('Click a cell to log time, e.g. 1h 30m, 45m or 1.5h (1d = 8h).')
          : t('Read only: people log their own time.')}
      </p>

      {cell && (
        <Popover anchor={cell.anchor} onClose={() => setCell(null)} width={320} className="ts-pop">
          <LogForm row={cell.row} date={cell.date} label={longDay.format(parseDay(cell.date)!)} onDone={() => setCell(null)} />
        </Popover>
      )}
    </div>
  )
}

const QUICK = ['15m', '30m', '1h', '2h', '4h', '1d']

function LogForm({ row, date, label, onDone }: { row: SheetRow; date: string; label: string; onDone: () => void }) {
  const me = useMe().data
  const qc = useQueryClient()
  const toast = useToast()
  const [time, setTime] = useState('')
  const [note, setNote] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const logs = useQuery({ queryKey: ['worklogs', row.key], queryFn: () => api.get<Worklog[]>(`/issues/${encodeURIComponent(row.key)}/worklogs`) })
  const mine = (logs.data ?? []).filter((w) => w.date === date && w.user?.id === me?.user?.id)
  const refresh = () => {
    for (const k of [['timesheet'], ['worklogs', row.key], ['issue', row.key], ['my-work'], ['search']]) qc.invalidateQueries({ queryKey: k })
  }
  const submit = async (e?: FormEvent) => {
    e?.preventDefault()
    if (!time.trim()) return setErr(t('How long? e.g. 1h 30m'))
    setBusy(true)
    setErr('')
    try {
      const r = await api.post<{ id: string; minutes: number }>(`/issues/${encodeURIComponent(row.key)}/worklogs`, { time: time.trim(), date, note: note.trim() || undefined })
      refresh()
      toast(t('Logged {time} on {key}', { time: duration(r.minutes), key: row.key }))
      onDone()
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  const remove = async (w: Worklog) => {
    try {
      await api.del(`/worklogs/${w.id}`)
      refresh()
    } catch (e) {
      setErr((e as Error).message)
    }
  }
  return (
    <form className="ts-form" onSubmit={submit}>
      <div className="ts-form-head">
        <span className="issue-key">{row.key}</span>
        <span className="ts-form-title">{row.title}</span>
      </div>
      <div className="ts-form-day">{label}</div>
      {logs.isLoading && <div className="faint small">{t('Loading…')}</div>}
      {mine.length > 0 && (
        <ul className="ts-logs" aria-label={t('Logged that day')}>
          {mine.map((w) => (
            <li key={w.id}>
              <strong>{duration(w.minutes)}</strong>
              <span className="ts-log-note">{w.note || <span className="faint">{t('No note')}</span>}</span>
              <button type="button" className="icon-btn icon-btn-sm" onClick={() => remove(w)} aria-label={t('Delete this entry')} title={t('Delete this entry')}><Trash size={13} /></button>
            </li>
          ))}
        </ul>
      )}
      <div className="field">
        <label htmlFor="ts-time">{mine.length ? t('Add time') : t('Time spent')}</label>
        <input id="ts-time" className="input" autoFocus value={time} onChange={(e) => setTime(e.target.value)} placeholder="1h 30m" autoComplete="off" />
        <div className="ts-quick">
          {QUICK.map((q) => (
            <button key={q} type="button" className={`chip ${time === q ? 'on' : ''}`} onClick={() => setTime(q)}>{q}</button>
          ))}
        </div>
      </div>
      <div className="field">
        <label htmlFor="ts-note">{t('Note (optional)')}</label>
        <input id="ts-note" className="input" value={note} onChange={(e) => setNote(e.target.value)} placeholder={t('What did you work on?')} autoComplete="off" />
      </div>
      {err && <p className="error-text ts-err" role="alert">{err}</p>}
      <div className="ts-form-foot">
        <button type="button" className="btn" onClick={onDone}>{t('Cancel')}</button>
        <button type="submit" className="btn btn-primary" disabled={busy || !time.trim()}>{t('Log time')}</button>
      </div>
    </form>
  )
}

function IssueSearch({ exclude, onPick }: { exclude: string[]; onPick: (i: Issue) => void }) {
  const [q, setQ] = useState('')
  const [active, setActive] = useState(0)
  const dq = useDebounced(q.trim(), 200)
  const found = useQuery({ queryKey: ['timesheet', 'quick', dq], queryFn: () => api.get<Issue[]>(`/quick${qs({ q: dq })}`), placeholderData: keepPreviousData })
  const list = (found.data ?? []).filter((i) => !exclude.includes(i.id))
  useEffect(() => setActive(0), [dq])
  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setActive((a) => Math.min(a + 1, list.length - 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setActive((a) => Math.max(a - 1, 0))
    } else if (e.key === 'Enter' && list[active]) {
      e.preventDefault()
      onPick(list[active])
    }
  }
  return (
    <div className="ts-search">
      <input className="input input-sm" autoFocus value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={onKey} placeholder={t('Search issues by key or words…')} aria-label={t('Search issues')} />
      <div className="menu-head">{dq ? t('Matching issues') : t('Recently viewed')}</div>
      <div className="choices-list" role="listbox">
        {list.map((i, n) => (
          <button key={i.id} type="button" role="option" aria-selected={n === active} className={`menu-item ${n === active ? 'hover' : ''}`} onMouseEnter={() => setActive(n)} onClick={() => onPick(i)}>
            <TypeIcon icon={i.type.icon} color={i.type.color} title={i.type.name} />
            <span className="issue-key">{i.key}</span>
            <span className="ts-search-title">{i.title}</span>
          </button>
        ))}
        {found.isLoading && <div className="menu-empty">{t('Searching…')}</div>}
        {found.data && list.length === 0 && <div className="menu-empty">{dq ? t('No issues found') : t('Search for an issue by key or title')}</div>}
      </div>
    </div>
  )
}
