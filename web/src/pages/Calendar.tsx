import { locale, t } from '../i18n'
import './Calendar.css'
import { useLayoutEffect, useMemo, useRef, useState, type DragEvent as ReactDragEvent, type KeyboardEvent as ReactKeyboardEvent } from 'react'
import { Link, useParams, useSearchParams } from 'react-router-dom'
import { keepPreviousData, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronLeft, ChevronRight, Inbox, PanelRightClose, PanelRightOpen, Plus, Search, TriangleExclamationPoint } from 'lucide-react'
import { api, isoDate, qs, shortDate, type Issue, type Priority, type Project } from '../api'
import { ErrorBox, Loading, PopButton, Spinner, useMe, useProject, useToast } from '../ui'
import { PriorityIcon, TypeIcon } from '../icons'
import { useCreateIssue, useOpenIssue } from '../components/Layout'
import { refreshIssues } from '../components/IssueView'

interface SearchResult {
  total: number
  issues: Issue[]
}

type Mode = 'due' | 'start'
type DateField = 'dueDate' | 'startDate'

// dates as whole days since 1970-01-01 (no time zones)
const DAY_MS = 86_400_000
const pad2 = (n: number) => String(n).padStart(2, '0')
const mod = (a: number, n: number) => ((a % n) + n) % n
function dnum(iso: string): number {
  const [y, m, d] = iso.slice(0, 10).split('-').map(Number)
  return Math.round(Date.UTC(y, m - 1, d) / DAY_MS)
}
function diso(n: number): string {
  const d = new Date(n * DAY_MS)
  return `${d.getUTCFullYear()}-${pad2(d.getUTCMonth() + 1)}-${pad2(d.getUTCDate())}`
}
const utc = (n: number) => new Date(n * DAY_MS)
const weekday = (n: number) => utc(n).getUTCDay()
function monthStart(n: number): number {
  const d = utc(n)
  return Math.round(Date.UTC(d.getUTCFullYear(), d.getUTCMonth(), 1) / DAY_MS)
}
function addMonths(n: number, k: number): number {
  const d = utc(n)
  return Math.round(Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + k, 1) / DAY_MS)
}
function parseMonth(s: string | null): number | null {
  const m = s?.match(/^(\d{4})-(\d{2})$/)
  if (!m) return null
  const mm = Number(m[2])
  return mm >= 1 && mm <= 12 ? Math.round(Date.UTC(Number(m[1]), mm - 1, 1) / DAY_MS) : null
}

const PRIO: Record<Priority, number> = { urgent: 0, high: 1, medium: 2, low: 3, none: 4 }
/** unfinished first, then by priority, then by key */
const chipOrder = (a: Issue, b: Issue) =>
  Number(a.status.category === 'done') - Number(b.status.category === 'done') || PRIO[a.priority] - PRIO[b.priority] || a.number - b.number

const MAX_CHIPS = 3
const SIDE_KEY = 'tc-calendar-side'

export default function Calendar() {
  const { key } = useParams()
  const project = useProject(key)
  if (project.isLoading) return <Loading />
  if (project.error) return <div className="page"><ErrorBox error={project.error} /></div>
  if (!project.data) return null
  return <CalendarView project={project.data} />
}

function CalendarView({ project }: { project: Project }) {
  const qc = useQueryClient()
  const toast = useToast()
  const open = useOpenIssue()
  const create = useCreateIssue()
  const me = useMe().data
  const meId = me?.user?.id
  const weekStart = me?.workspace?.weekStart === 'sunday' ? 0 : 1
  const canEdit = project.role === 'member' || project.role === 'admin'
  const [params, setParams] = useSearchParams()
  const today = dnum(isoDate(new Date()))
  const month = parseMonth(params.get('month')) ?? monthStart(today)
  const mode: Mode = params.get('by') === 'start' ? 'start' : 'due'
  const field: DateField = mode === 'due' ? 'dueDate' : 'startDate'
  const tql = mode === 'due' ? 'due' : 'start'
  const setParam = (k: string, v: string | null) => {
    const p = new URLSearchParams(params)
    if (v) p.set(k, v)
    else p.delete(k)
    setParams(p, { replace: true })
  }
  const goMonth = (n: number) => setParam('month', n === monthStart(today) ? null : diso(n).slice(0, 7))

  const [text, setText] = useState('')
  const [mine, setMine] = useState(false)
  const [showDone, setShowDone] = useState(true)
  const [side, setSideState] = useState(() => {
    try {
      return localStorage.getItem(SIDE_KEY) !== '0'
    } catch {
      return true
    }
  })
  const setSide = (v: boolean) => {
    setSideState(v)
    try {
      localStorage.setItem(SIDE_KEY, v ? '1' : '0')
    } catch {
      /* storage unavailable */
    }
  }
  const [drag, setDrag] = useState<{ key: string; from: 'grid' | 'side' } | null>(null)
  const [over, setOver] = useState<number | 'side' | null>(null)
  const root = useRef<HTMLDivElement>(null)
  const refocus = useRef<{ key: string; until: number } | null>(null)
  const saving = useRef(new Map<string, Promise<void>>())

  // the visible weeks
  const monthEnd = addMonths(month, 1) - 1
  const gridStart = month - mod(weekday(month) - weekStart, 7)
  const gridEnd = monthEnd + mod(weekStart + 6 - weekday(monthEnd), 7)
  const days = useMemo(() => Array.from({ length: gridEnd - gridStart + 1 }, (_, k) => gridStart + k), [gridStart, gridEnd])

  const rangeKey = useMemo(() => ['calendar', project.key, mode, diso(gridStart), diso(gridEnd)], [project.key, mode, gridStart, gridEnd])
  const unschedKey = useMemo(() => ['calendar', project.key, mode, 'unscheduled'], [project.key, mode])
  const data = useQuery({
    queryKey: rangeKey,
    queryFn: () => api.get<SearchResult>(`/search${qs({ project: project.key, limit: 1000, order: 'priority', q: `${tql} >= ${diso(gridStart)} and ${tql} <= ${diso(gridEnd)}` })}`),
    placeholderData: keepPreviousData,
  })
  const unsched = useQuery({
    queryKey: unschedKey,
    queryFn: () => api.get<SearchResult>(`/search${qs({ project: project.key, limit: 300, order: 'rank', q: `${tql} is empty and category != done` })}`),
  })

  const q = text.trim().toLowerCase()
  const visible = (i: Issue) => (!q || `${i.key} ${i.title}`.toLowerCase().includes(q)) && (!mine || i.assignee?.id === meId) && (showDone || i.status.category !== 'done')
  const byDay = useMemo(() => {
    const m = new Map<number, Issue[]>()
    for (const i of data.data?.issues ?? []) {
      const v = i[field]
      if (!v || !visible(i)) continue
      const n = dnum(v)
      if (n < gridStart || n > gridEnd) continue
      const l = m.get(n)
      if (l) l.push(i)
      else m.set(n, [i])
    }
    for (const l of m.values()) l.sort(chipOrder)
    return m
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data.data, field, gridStart, gridEnd, q, mine, showDone, meId])
  const sideList = (unsched.data?.issues ?? []).filter(visible)
  const inMonth = [...byDay.entries()].reduce((s, [n, l]) => s + (n >= month && n <= monthEnd ? l.length : 0), 0)

  const fmt = useMemo(() => {
    const l = locale()
    return {
      title: new Intl.DateTimeFormat(l, { month: 'long', year: 'numeric', timeZone: 'UTC' }),
      weekday: new Intl.DateTimeFormat(l, { weekday: 'short', timeZone: 'UTC' }),
      full: new Intl.DateTimeFormat(l, { weekday: 'long', day: 'numeric', month: 'long', timeZone: 'UTC' }),
      dayMonth: new Intl.DateTimeFormat(l, { day: 'numeric', month: 'short', timeZone: 'UTC' }),
    }
  }, [])
  const weekdays = days.slice(0, 7)

  // ---- changing a date (optimistic, then PATCH) ----
  const find = (key: string) => data.data?.issues.find((i) => i.key === key) ?? unsched.data?.issues.find((i) => i.key === key)
  const moveTo = async (issue: Issue, date: string | null, keyboard = false) => {
    const old = issue[field]
    if (old === date) return
    const patch: { startDate?: string | null; dueDate?: string | null } = { [field]: date }
    // keep start ≤ due: the other date moves along when it would end up on the wrong side
    if (date) {
      const n = dnum(date)
      const delta = old ? n - dnum(old) : 0
      if (mode === 'due' && issue.startDate && dnum(issue.startDate) > n) patch.startDate = old ? diso(dnum(issue.startDate) + delta) : date
      if (mode === 'start' && issue.dueDate && dnum(issue.dueDate) < n) patch.dueDate = old ? diso(dnum(issue.dueDate) + delta) : date
    }
    const updated: Issue = { ...issue, ...patch }
    const shown = date != null && dnum(date) >= gridStart && dnum(date) <= gridEnd
    if (keyboard) refocus.current = { key: issue.key, until: Date.now() + 1500 }
    // a refetch in flight would overwrite the optimistic change
    if (qc.isFetching({ queryKey: ['calendar', project.key] })) await qc.cancelQueries({ queryKey: ['calendar', project.key] })
    qc.setQueryData<SearchResult>(rangeKey, (o) => {
      if (!o) return o
      const had = o.issues.some((x) => x.id === issue.id)
      return { total: o.total - (had ? 1 : 0) + (shown ? 1 : 0), issues: [...o.issues.filter((x) => x.id !== issue.id), ...(shown ? [updated] : [])] }
    })
    qc.setQueryData<SearchResult>(unschedKey, (o) => {
      if (!o) return o
      const had = o.issues.some((x) => x.id === issue.id)
      const add = !date && issue.status.category !== 'done'
      return { total: o.total - (had ? 1 : 0) + (add ? 1 : 0), issues: [...(add ? [updated] : []), ...o.issues.filter((x) => x.id !== issue.id)] }
    })
    // one request at a time per issue, in order (quick keyboard moves)
    const run = async () => {
      try {
        await api.patch(`/issues/${encodeURIComponent(issue.key)}`, patch)
        qc.invalidateQueries({ queryKey: ['issue', issue.key] })
        refreshIssues(qc)
      } catch (e) {
        toast(t('Couldn’t change the date of {key}: {error}', { key: issue.key, error: (e as Error).message }))
        qc.invalidateQueries({ queryKey: ['calendar', project.key] })
      }
    }
    const next = (saving.current.get(issue.key) ?? Promise.resolve()).then(run)
    saving.current.set(issue.key, next)
    await next
    if (saving.current.get(issue.key) === next) saving.current.delete(issue.key)
  }
  // keyboard moves re-create the chip in another day: keep the focus on it
  useLayoutEffect(() => {
    const r = refocus.current
    if (!r) return
    if (Date.now() > r.until) {
      refocus.current = null
      return
    }
    const el = root.current?.querySelector<HTMLElement>(`.cal-grid [data-cal-key="${CSS.escape(r.key)}"]`)
    if (el && document.activeElement !== el) el.focus()
  })

  const onChipKey = (e: ReactKeyboardEvent<HTMLElement>, shown: Issue) => {
    if (!canEdit || !e.altKey || !['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(e.key)) return
    // quick key presses: start from the latest (optimistic) dates, not the ones this chip was drawn with
    const issue = qc.getQueryData<SearchResult>(rangeKey)?.issues.find((x) => x.id === shown.id) ?? shown
    const cur = issue[field]
    if (!cur) return
    e.preventDefault()
    const rtl = getComputedStyle(e.currentTarget).direction === 'rtl'
    const step = e.key === 'ArrowUp' ? -7 : e.key === 'ArrowDown' ? 7 : (e.key === 'ArrowLeft') !== rtl ? -1 : 1
    const next = dnum(cur) + step
    if (next < gridStart || next > gridEnd) {
      if (next < month) goMonth(addMonths(month, -1))
      else if (next > monthEnd) goMonth(addMonths(month, 1))
    }
    void moveTo(issue, diso(next), true)
  }

  const dragProps = (issue: Issue, from: 'grid' | 'side') =>
    canEdit
      ? {
          draggable: true,
          onDragStart: (e: ReactDragEvent<HTMLElement>) => {
            e.dataTransfer.effectAllowed = 'move'
            e.dataTransfer.setData('text/plain', issue.key)
            setDrag({ key: issue.key, from })
          },
          onDragEnd: () => {
            setDrag(null)
            setOver(null)
          },
        }
      : {}

  const chip = (issue: Issue, from: 'grid' | 'side', extra = false) => {
    const cat = issue.status.category
    const overdue = cat !== 'done' && !!issue.dueDate && dnum(issue.dueDate) < today
    const dates = issue.startDate && issue.dueDate ? `${shortDate(issue.startDate)} – ${shortDate(issue.dueDate)}` : issue.dueDate ? t('Due {date}', { date: shortDate(issue.dueDate) }) : issue.startDate ? t('Starts {date}', { date: shortDate(issue.startDate) }) : ''
    return (
      <button
        key={issue.id}
        type="button"
        className={`cal-chip cal-${cat} ${overdue ? 'cal-late' : ''} ${drag?.key === issue.key ? 'cal-dragging' : ''} ${extra ? 'cal-chip-extra' : ''} ${from === 'side' ? 'cal-chip-side' : ''}`}
        data-cal-key={issue.key}
        title={`${issue.key} ${issue.title}\n${issue.status.name}${dates ? ` · ${dates}` : ''}${issue.assignee ? ` · ${issue.assignee.name}` : ''}${overdue ? `\n${t('Overdue')}` : ''}`}
        aria-label={`${issue.key} ${issue.title}, ${issue.status.name}${overdue ? `, ${t('Overdue')}` : ''}`}
        onClick={() => open(issue.key)}
        onKeyDown={(e) => onChipKey(e, issue)}
        {...dragProps(issue, from)}
      >
        <TypeIcon icon={issue.type.icon} color={issue.type.color} size={14} title={issue.type.name} />
        <span className="issue-key">{issue.key}</span>
        <span className="cal-chip-title" dir="auto">{issue.title}</span>
        {from === 'side' && <PriorityIcon priority={issue.priority} size={14} />}
        {overdue && (
          <span className="cal-overdue" title={t('Overdue')}>
            <TriangleExclamationPoint size={12} aria-hidden />
          </span>
        )}
      </button>
    )
  }

  const dropOn = (target: number | 'side') => ({
    onDragOver: (e: ReactDragEvent<HTMLElement>) => {
      if (!drag || !canEdit || (target === 'side' && drag.from === 'side')) return
      e.preventDefault()
      e.dataTransfer.dropEffect = 'move'
      if (over !== target) setOver(target)
    },
    onDragLeave: (e: ReactDragEvent<HTMLElement>) => {
      if (!e.currentTarget.contains(e.relatedTarget as Node)) setOver((o) => (o === target ? null : o))
    },
    onDrop: (e: ReactDragEvent<HTMLElement>) => {
      e.preventDefault()
      const k = drag?.key ?? e.dataTransfer.getData('text/plain')
      setOver(null)
      setDrag(null)
      const issue = k ? find(k) : undefined
      if (issue) void moveTo(issue, target === 'side' ? null : diso(target))
    },
  })

  const newIssue = (n: number) => create({ projectKey: project.key, ...(mode === 'due' ? { dueDate: diso(n) } : { startDate: diso(n) }) })

  return (
    <div className="cal-wrap" ref={root}>
      <div className="crumbs">
        <Link to="/projects">{t('Projects')}</Link>
        <span>/</span>
        <Link to={`/projects/${project.key}`}>{project.name}</Link>
      </div>
      <div className="page-head cal-pagehead">
        <h1>{t('Calendar')}</h1>
        <div className="cal-nav">
          <button className="icon-btn" onClick={() => goMonth(addMonths(month, -1))} aria-label={t('Previous month')} title={t('Previous month')}>
            <ChevronLeft size={18} className="cal-flip" />
          </button>
          <button className="btn btn-sm" onClick={() => goMonth(monthStart(today))} disabled={month === monthStart(today)}>
            {t('Today')}
          </button>
          <button className="icon-btn" onClick={() => goMonth(addMonths(month, 1))} aria-label={t('Next month')} title={t('Next month')}>
            <ChevronRight size={18} className="cal-flip" />
          </button>
          <h2 className="cal-title" aria-live="polite">{fmt.title.format(utc(month))}</h2>
          {data.isFetching && <Spinner size={14} />}
        </div>
        <span className="spacer" />
        <div className="seg" role="group" aria-label={t('Show issues by')}>
          <button className={mode === 'due' ? 'active' : ''} aria-pressed={mode === 'due'} onClick={() => setParam('by', null)}>
            {t('Due date')}
          </button>
          <button className={mode === 'start' ? 'active' : ''} aria-pressed={mode === 'start'} onClick={() => setParam('by', 'start')}>
            {t('Start date')}
          </button>
        </div>
      </div>
      <div className="toolbar">
        <div className="row cal-search">
          <Search size={14} className="faint" aria-hidden />
          <input className="input input-sm" placeholder={t('Filter issues…')} value={text} onChange={(e) => setText(e.target.value)} aria-label={t('Filter issues')} />
        </div>
        {meId && (
          <button className={`chip ${mine ? 'on' : ''}`} aria-pressed={mine} onClick={() => setMine((m) => !m)}>
            {t('Only my issues')}
          </button>
        )}
        <button className={`chip ${showDone ? 'on' : ''}`} aria-pressed={showDone} onClick={() => setShowDone((v) => !v)}>
          {t('Show done')}
        </button>
        <span className="spacer" />
        <button className="btn btn-sm" onClick={() => setSide(!side)} aria-pressed={side}>
          {side ? <PanelRightClose size={14} /> : <PanelRightOpen size={14} />} {t('Unscheduled')}
          {unsched.data && <span className="badge">{unsched.data.total}</span>}
        </button>
      </div>

      {data.error && <ErrorBox error={data.error} />}

      <div className={`cal-layout ${side ? '' : 'cal-no-side'}`}>
        <div className={`cal-month ${!data.data ? 'cal-loading' : ''}`}>
          <div className="cal-weekdays" aria-hidden>
            {weekdays.map((n) => (
              <div key={n} className={weekday(n) === 0 || weekday(n) === 6 ? 'cal-weekend' : ''}>{fmt.weekday.format(utc(n))}</div>
            ))}
          </div>
          <div className="cal-grid">
            {days.map((n) => {
              const list = byDay.get(n) ?? []
              const shownChips = list.length <= MAX_CHIPS ? list : list.slice(0, MAX_CHIPS - 1)
              const rest = list.slice(shownChips.length)
              const wd = weekday(n)
              const other = n < month || n > monthEnd
              const label = fmt.full.format(utc(n))
              return (
                <section
                  key={n}
                  className={`cal-day ${other ? 'cal-other' : ''} ${n === today ? 'cal-today' : ''} ${wd === 0 || wd === 6 ? 'cal-weekend' : ''} ${list.length ? '' : 'cal-empty'} ${over === n ? 'cal-drop' : ''} ${n < today ? 'cal-past' : ''}`}
                  aria-label={label}
                  {...dropOn(n)}
                >
                  <div className="cal-day-head">
                    <span className="cal-num" aria-hidden>{utc(n).getUTCDate() === 1 || n === gridStart ? fmt.dayMonth.format(utc(n)) : utc(n).getUTCDate()}</span>
                    <span className="cal-full">{label}</span>
                    {n === today && <span className="cal-today-badge">{t('Today')}</span>}
                    <span className="spacer" />
                    {canEdit && (
                      <button className="icon-btn icon-btn-sm cal-add" onClick={() => newIssue(n)} aria-label={t('Create an issue on {date}', { date: label })} title={t('Create an issue on {date}', { date: label })}>
                        <Plus size={14} />
                      </button>
                    )}
                  </div>
                  <div className="cal-chips">
                    {shownChips.map((i) => chip(i, 'grid'))}
                    {rest.map((i) => chip(i, 'grid', true))}
                    {rest.length > 0 && (
                      <PopButton className="cal-more" label={t('+{n} more', { n: rest.length })} title={t('{n} more on {date}', { n: rest.length, date: label })} width={300}>
                        {() => (
                          <div className="cal-pop">
                            <div className="cal-pop-head">{label}</div>
                            {list.map((i) => chip(i, 'grid'))}
                          </div>
                        )}
                      </PopButton>
                    )}
                  </div>
                </section>
              )
            })}
          </div>
          {data.data && inMonth === 0 && (
            <div className="cal-month-empty">
              {mode === 'due' ? t('Nothing is due this month.') : t('Nothing starts this month.')}
            </div>
          )}
        </div>

        {side && (
          <aside className={`cal-side ${over === 'side' ? 'cal-drop' : ''} ${drag?.from === 'grid' ? 'cal-can-drop' : ''}`} aria-label={t('Unscheduled')} {...dropOn('side')}>
            <div className="cal-side-head">
              <Inbox size={15} aria-hidden />
              <strong>{t('Unscheduled')}</strong>
              {unsched.data && <span className="badge">{unsched.data.total}</span>}
            </div>
            <p className="faint small cal-side-hint">
              {mode === 'due' ? t('Open issues without a due date.') : t('Open issues without a start date.')} {canEdit && t('Drag one onto a day to schedule it.')}
            </p>
            {drag?.from === 'grid' && <div className="cal-side-drop">{mode === 'due' ? t('Drop here to remove the due date') : t('Drop here to remove the start date')}</div>}
            {unsched.isLoading && <Loading />}
            {unsched.error && <ErrorBox error={unsched.error} />}
            <div className="cal-side-list">
              {sideList.map((i) => chip(i, 'side'))}
              {unsched.data && sideList.length === 0 && <div className="faint small cal-side-empty">{q || mine ? t('No issues match the filters') : t('Everything open has a date.')}</div>}
              {unsched.data && unsched.data.total > unsched.data.issues.length && (
                <div className="faint small cal-side-empty">{t('Showing the first {n}', { n: unsched.data.issues.length })}</div>
              )}
            </div>
          </aside>
        )}
      </div>
    </div>
  )
}
