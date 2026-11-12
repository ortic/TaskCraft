import { locale, t } from '../i18n'
import './Timeline.css'
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent as ReactKeyboardEvent, type PointerEvent as ReactPointerEvent } from 'react'
import { Link, useParams } from 'react-router-dom'
import { keepPreviousData, useQuery, useQueryClient } from '@tanstack/react-query'
import { CalendarPlus, ChartGantt, ChevronDown, ChevronRight, ChevronsDownUp, ChevronsUpDown, Crosshair, Keyboard, PanelLeftClose, PanelLeftOpen, Plus, Search, TriangleExclamationPoint } from 'lucide-react'
import { api, isoDate, qs, shortDate, type Issue, type IssueDetail, type Project } from '../api'
import { Empty, ErrorBox, Loading, PopButton, Spinner, useMe, useProject, useToast } from '../ui'
import { TypeIcon } from '../icons'
import { useCreateIssue, useOpenIssue } from '../components/Layout'
import { refreshIssues } from '../components/IssueView'

// ---- types ----

interface SearchResult {
  total: number
  issues: Issue[]
}

type Zoom = 'day' | 'week' | 'month'
type DragMode = 'move' | 'start' | 'end'
type Span = [number | null, number | null]

/** A "blocks" link: `from` blocks `to`. `snap` keeps both issues as the link payload had them (for issues not on the timeline). */
interface Dep {
  from: string
  to: string
  snap: Record<string, Issue>
}

type Row =
  | { kind: 'issue'; issue: Issue; depth: number; context: boolean; group?: string; count?: number; rollup?: [number, number] | null }
  | { kind: 'group'; id: string; label: string; parentKey?: string; color?: string; count: number; rollup: [number, number] | null }

interface Group {
  id: string
  head: Row
  items: Row[]
}

type Geo = { kind: 'bar'; x: number; w: number; open: boolean } | { kind: 'diamond'; x: number }

interface Prefs {
  zoom: Zoom
  showDone: boolean
  compact: boolean
}

// ---- constants & date helpers (dates as whole days since 1970-01-01, no time zones) ----

const DAY_MS = 86_400_000
const ROW_H = 36
const PPD: Record<Zoom, number> = { day: 34, week: 15, month: 4.5 }
/** at most this many issue details are fetched for dependencies */
const MAX_LINKED = 60
const QUERY = 'category != done or resolved >= -30d'
const PREFS_KEY = 'tc-timeline'

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
function nextMonth(n: number): number {
  const d = utc(n)
  return Math.round(Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 1) / DAY_MS)
}
const spanOfIssue = (i: Issue): Span => [i.startDate ? dnum(i.startDate) : null, i.dueDate ? dnum(i.dueDate) : null]

/** The dates after dragging/resizing by `delta` days (start never after due). */
function shifted([s, d]: Span, mode: DragMode, delta: number): Span {
  if (mode === 'move') return [s == null ? null : s + delta, d == null ? null : d + delta]
  if (mode === 'start') {
    const base = s ?? d
    if (base == null) return [s, d]
    const ns = base + delta
    return [d != null && ns > d ? d : ns, d]
  }
  const base = d ?? s
  if (base == null) return [s, d]
  const nd = base + delta
  return [s, s != null && nd < s ? s : nd]
}

function geometry([s, d]: Span, start: number, ppd: number): Geo | null {
  if (s != null && d != null) {
    const a = Math.min(s, d)
    const b = Math.max(s, d)
    return { kind: 'bar', x: (a - start) * ppd, w: (b - a + 1) * ppd, open: false }
  }
  if (s != null) return { kind: 'bar', x: (s - start) * ppd, w: Math.max(ppd, 28), open: true }
  if (d != null) return { kind: 'diamond', x: (d - start + 0.5) * ppd }
  return null
}
const edges = (g: Geo): [number, number] => (g.kind === 'bar' ? [g.x, g.x + g.w] : [g.x - 8, g.x + 8])

/** A blocked issue that starts before its (unfinished) blocker ends. */
function isConflict(blocker: Issue, blocked: Issue, a: Span, b: Span): boolean {
  if (blocker.status.category === 'done' || blocked.status.category === 'done') return false
  const end = a[1] ?? a[0]
  const start = b[0] ?? b[1]
  return end != null && start != null && start <= end
}

function arrowPath(x1: number, y1: number, x2: number, y2: number): string {
  if (x2 - x1 >= 24) {
    const c = Math.min(40, (x2 - x1) / 2)
    return `M${x1} ${y1} C${x1 + c} ${y1} ${x2 - c} ${y2} ${x2} ${y2}`
  }
  const ym = y1 + (y2 > y1 ? ROW_H / 2 : -ROW_H / 2)
  return `M${x1} ${y1} h10 V${ym} H${x2 - 10} V${y2} H${x2}`
}

function computeRange(lo: number, hi: number, today: number, zoom: Zoom, weekStart: number) {
  const pad = zoom === 'month' ? 75 : zoom === 'week' ? 35 : 14
  let a = Math.min(Number.isFinite(lo) ? lo : today, today - (zoom === 'day' ? 14 : 30)) - pad
  let b = Math.max(Number.isFinite(hi) ? hi : today, today + (zoom === 'day' ? 45 : zoom === 'week' ? 90 : 240)) + pad
  a = Math.max(a, today - 3 * 365)
  b = Math.min(b, today + 4 * 365)
  if (zoom === 'month') {
    a = monthStart(a)
    b = nextMonth(b) - 1
  } else {
    a -= mod(weekday(a) - weekStart, 7)
    b += mod(weekStart + 6 - weekday(b), 7)
  }
  return { start: a, days: b - a + 1 }
}

/** Runs `f` over `items`, at most `n` at a time. */
async function pool<T, R>(items: T[], n: number, f: (x: T) => Promise<R>): Promise<R[]> {
  const out: R[] = new Array(items.length)
  let next = 0
  await Promise.all(
    Array.from({ length: Math.min(n, items.length) }, async () => {
      while (next < items.length) {
        const k = next++
        out[k] = await f(items[k])
      }
    }),
  )
  return out
}

function loadPrefs(): Partial<Prefs> {
  try {
    const p = JSON.parse(localStorage.getItem(PREFS_KEY) ?? '{}')
    return p && typeof p === 'object' ? p : {}
  } catch {
    return {}
  }
}

function savePrefs(p: Prefs) {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify(p))
  } catch {
    /* storage unavailable */
  }
}

function dateText([s, d]: Span): string {
  if (s != null && d != null) return `${shortDate(diso(Math.min(s, d)))} – ${shortDate(diso(Math.max(s, d)))}`
  if (s != null) return t('Starts {date}', { date: shortDate(diso(s)) })
  if (d != null) return t('Due {date}', { date: shortDate(diso(d)) })
  return t('No dates')
}

const zoomLabel = (z: Zoom) => (z === 'day' ? t('Days') : z === 'week' ? t('Weeks') : t('Months'))

// ---- page ----

export default function Timeline() {
  const { key } = useParams()
  const project = useProject(key)
  const pkey = project.data?.key
  const data = useQuery({
    queryKey: ['timeline', pkey, 'issues'],
    queryFn: () => api.get<SearchResult>(`/search${qs({ project: pkey, limit: 1000, order: 'rank', q: QUERY })}`),
    enabled: !!pkey,
  })
  if (project.isLoading || (pkey && data.isLoading)) return <Loading />
  if (project.error) return <div className="page"><ErrorBox error={project.error} /></div>
  if (data.error) return <div className="page"><ErrorBox error={data.error} /></div>
  if (!project.data || !data.data) return null
  return <TimelineView project={project.data} result={data.data} />
}

function TimelineView({ project, result }: { project: Project; result: SearchResult }) {
  const qc = useQueryClient()
  const toast = useToast()
  const open = useOpenIssue()
  const create = useCreateIssue()
  const me = useMe().data
  const meId = me?.user?.id
  const weekStart = me?.workspace?.weekStart === 'sunday' ? 0 : 1
  const canEdit = project.role === 'member' || project.role === 'admin'
  const issuesKey = useMemo(() => ['timeline', project.key, 'issues'], [project.key])
  const issues = result.issues
  const uid = useId().replace(/[^a-zA-Z0-9_-]/g, '')

  const [prefs, setPrefsState] = useState<Prefs>(() => ({ zoom: 'week', showDone: true, compact: false, ...loadPrefs() }))
  const setPrefs = (p: Partial<Prefs>) =>
    setPrefsState((old) => {
      const n = { ...old, ...p }
      savePrefs(n)
      return n
    })
  const zoom: Zoom = PPD[prefs.zoom] ? prefs.zoom : 'week'
  const { showDone, compact } = prefs
  const ppd = PPD[zoom]
  const [text, setText] = useState('')
  const [mine, setMine] = useState(false)
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set())
  const [drag, setDrag] = useState<{ key: string; mode: DragMode; delta: number } | null>(null)
  const dragRef = useRef<{ issue: Issue; mode: DragMode; x0: number; moved: boolean } | null>(null)
  const scroller = useRef<HTMLDivElement>(null)
  const corner = useRef<HTMLDivElement>(null)
  const [, setTick] = useState(0)
  const today = dnum(isoDate(new Date()))

  // ---- time range ----
  let lo = Infinity
  let hi = -Infinity
  for (const i of issues)
    for (const d of [i.startDate, i.dueDate])
      if (d) {
        const n = dnum(d)
        if (n < lo) lo = n
        if (n > hi) hi = n
      }
  const range = useMemo(() => computeRange(lo, hi, today, zoom, weekStart), [lo, hi, today, zoom, weekStart])
  const W = range.days * ppd
  const end = range.start + range.days

  // keep the view steady: initial position, zoom changes (same centre date), and range growing to the left
  const pending = useRef<{ day: number; frac: number; smooth?: boolean } | null>({ day: today, frac: 0.25 })
  const prev = useRef<{ start: number; ppd: number } | null>(null)
  useLayoutEffect(() => {
    const el = scroller.current
    if (!el) return
    const view = el.clientWidth - (corner.current?.offsetWidth ?? 0)
    if (pending.current) {
      const { day, frac, smooth } = pending.current
      pending.current = null
      el.scrollTo({ left: Math.max(0, (day - range.start) * ppd - view * frac), behavior: smooth ? 'smooth' : 'auto' })
    } else if (prev.current && prev.current.ppd === ppd && prev.current.start !== range.start) {
      el.scrollLeft += (prev.current.start - range.start) * ppd
    }
    prev.current = { start: range.start, ppd }
  })
  const viewDays = () => {
    const el = scroller.current
    if (!el) return { first: today, last: today, center: today }
    const view = el.clientWidth - (corner.current?.offsetWidth ?? 0)
    const first = range.start + el.scrollLeft / ppd
    return { first: Math.ceil(first), last: Math.floor(first + view / ppd) - 1, center: first + view / 2 / ppd }
  }
  const changeZoom = (z: Zoom) => {
    if (z === zoom) return
    pending.current = { day: viewDays().center, frac: 0.5 }
    setPrefs({ zoom: z })
  }
  const goToday = () => {
    pending.current = { day: today, frac: 0.25, smooth: true }
    setTick((n) => n + 1)
  }

  // ---- rows: epics with their issues, then issues of epics not shown, then issues without an epic ----
  const groups = useMemo(() => {
    const inList = new Set(issues.map((i) => i.id))
    const kids = new Map<string, Issue[]>()
    const loose: Issue[] = []
    for (const i of issues) {
      if (i.type.level >= 1) continue
      if (i.parent && inList.has(i.parent.id)) {
        const l = kids.get(i.parent.id)
        if (l) l.push(i)
        else kids.set(i.parent.id, [i])
      } else loose.push(i)
    }
    const q = text.trim().toLowerCase()
    const textHit = (s: string) => !q || s.toLowerCase().includes(q)
    const other = (i: Issue) => (showDone || i.status.category !== 'done') && (!mine || i.assignee?.id === meId)
    // the issues that pass the filters, with the parents they need shown as context
    const walk = (list: Issue[], depth: number, inherited: boolean): Row[] => {
      const out: Row[] = []
      for (const i of list) {
        const own = textHit(`${i.key} ${i.title}`)
        const hit = (inherited || own) && other(i)
        const sub = depth < 3 ? walk(kids.get(i.id) ?? [], depth + 1, inherited || (!!q && own)) : []
        if (hit || sub.length) out.push({ kind: 'issue', issue: i, depth, context: !hit }, ...sub)
      }
      return out
    }
    const descendants = (id: string): Issue[] => {
      const out: Issue[] = []
      const go = (pid: string, depth: number) => {
        for (const c of kids.get(pid) ?? []) {
          out.push(c)
          if (depth < 3) go(c.id, depth + 1)
        }
      }
      go(id, 0)
      return out
    }
    const spanOf = (list: Issue[]): [number, number] | null => {
      let a = Infinity
      let b = -Infinity
      for (const i of list)
        for (const d of [i.startDate, i.dueDate])
          if (d) {
            const n = dnum(d)
            a = Math.min(a, n)
            b = Math.max(b, n)
          }
      return Number.isFinite(a) ? [a, b] : null
    }
    const out: Group[] = []
    for (const e of issues) {
      if (e.type.level < 1) continue
      const own = textHit(`${e.key} ${e.title}`)
      const items = walk(kids.get(e.id) ?? [], 1, !!q && own)
      const hit = own && other(e)
      if (!hit && !items.length) continue
      out.push({ id: e.id, head: { kind: 'issue', issue: e, depth: 0, context: !hit, group: e.id, count: items.length, rollup: spanOf(descendants(e.id)) }, items })
    }
    // issues whose epic isn't on the timeline (finished long ago, or in another project)
    const byParent = new Map<string, { ref: NonNullable<Issue['parent']>; list: Issue[] }>()
    const none: Issue[] = []
    for (const i of loose) {
      if (i.parent && i.type.level === 0) {
        const g = byParent.get(i.parent.id)
        if (g) g.list.push(i)
        else byParent.set(i.parent.id, { ref: i.parent, list: [i] })
      } else none.push(i)
    }
    for (const [id, g] of byParent) {
      const items = walk(g.list, 1, !!q && textHit(`${g.ref.key} ${g.ref.title}`))
      if (!items.length) continue
      out.push({
        id,
        head: { kind: 'group', id, label: g.ref.title, parentKey: g.ref.key, color: g.ref.color ?? undefined, count: items.length, rollup: spanOf(g.list.flatMap((i) => [i, ...descendants(i.id)])) },
        items,
      })
    }
    const noneItems = walk(none, 1, false)
    if (noneItems.length) out.push({ id: 'none', head: { kind: 'group', id: 'none', label: out.length ? t('Issues without an epic') : t('Issues'), count: noneItems.length, rollup: null }, items: noneItems })
    return out
  }, [issues, text, showDone, mine, meId])
  const rows = useMemo(() => groups.flatMap((g) => (collapsed.has(g.id) ? [g.head] : [g.head, ...g.items])), [groups, collapsed])
  const toggle = (id: string) =>
    setCollapsed((s) => {
      const n = new Set(s)
      if (n.has(id)) n.delete(id)
      else n.add(id)
      return n
    })
  const allCollapsed = groups.length > 0 && groups.every((g) => collapsed.has(g.id) || !g.items.length)

  // ---- dependencies: the "blocks" links of blocked issues (bounded; unchanged issues come from a cache) ----
  const blockedStamps = useMemo(
    () =>
      issues
        .filter((i) => i.blocked)
        .sort((a, b) => Number(!!(b.startDate || b.dueDate)) - Number(!!(a.startDate || a.dueDate)))
        .slice(0, MAX_LINKED)
        .map((i) => `${i.key}@${i.updatedAt}`),
    [issues],
  )
  const linkCache = useRef(new Map<string, Dep[]>())
  const links = useQuery({
    queryKey: ['timeline', project.key, 'links', blockedStamps.join(',')],
    enabled: blockedStamps.length > 0,
    placeholderData: keepPreviousData,
    staleTime: 30_000,
    queryFn: async () => {
      const lists = await pool(blockedStamps, 6, async (stamp) => {
        const hit = linkCache.current.get(stamp)
        if (hit) return hit
        const k = stamp.slice(0, stamp.indexOf('@'))
        try {
          const d = await api.get<IssueDetail>(`/issues/${encodeURIComponent(k)}`)
          const deps: Dep[] = d.links
            .filter((l) => l.kind === 'blocks')
            .map((l) => ({ from: l.outward ? d.key : l.issue.key, to: l.outward ? l.issue.key : d.key, snap: { [d.key]: d, [l.issue.key]: l.issue } }))
          linkCache.current.set(stamp, deps)
          return deps
        } catch {
          return []
        }
      })
      const seen = new Map<string, Dep>()
      for (const l of lists) for (const d of l) seen.set(`${d.from}>${d.to}`, d)
      return [...seen.values()]
    },
  })
  const deps = blockedStamps.length ? links.data ?? [] : []
  const byKey = useMemo(() => new Map(issues.map((i) => [i.key, i])), [issues])
  const current = (k: string, d: Dep) => byKey.get(k) ?? d.snap[k]
  const conflicts = deps.filter((d) => {
    const a = current(d.from, d)
    const b = current(d.to, d)
    return !!a && !!b && isConflict(a, b, spanOfIssue(a), spanOfIssue(b))
  })
  const conflictKeys = new Set(conflicts.map((c) => c.to))

  // ---- changing dates (optimistic, then PATCH) ----
  const timers = useRef(new Map<string, { timer: ReturnType<typeof setTimeout>; run: () => void }>())
  /** dates of keyboard moves not saved yet (quick key presses build on these) */
  const nudged = useRef(new Map<string, Span>())
  const persist = async (key: string, startDate: string | null, dueDate: string | null) => {
    try {
      await api.patch(`/issues/${encodeURIComponent(key)}`, { startDate, dueDate })
      qc.invalidateQueries({ queryKey: ['issue', key] })
      refreshIssues(qc)
    } catch (e) {
      toast(t('Couldn’t change the dates of {key}: {error}', { key, error: (e as Error).message }))
      qc.invalidateQueries({ queryKey: issuesKey })
    }
  }
  useEffect(() => {
    const pendingSaves = timers.current
    return () => {
      // leaving the page: save what's still waiting
      for (const p of pendingSaves.values()) {
        clearTimeout(p.timer)
        p.run()
      }
      pendingSaves.clear()
    }
  }, [])
  const setDates = async (issue: Issue, span: Span, debounce = false) => {
    const startDate = span[0] == null ? null : diso(span[0])
    const dueDate = span[1] == null ? null : diso(span[1])
    if (startDate === issue.startDate && dueDate === issue.dueDate) {
      setDrag(null)
      return
    }
    if (qc.isFetching({ queryKey: issuesKey })) await qc.cancelQueries({ queryKey: issuesKey })
    qc.setQueryData<SearchResult>(issuesKey, (old) => old && { ...old, issues: old.issues.map((x) => (x.id === issue.id ? { ...x, startDate, dueDate } : x)) })
    setDrag(null)
    const waiting = timers.current.get(issue.key)
    if (waiting) clearTimeout(waiting.timer)
    const run = () => {
      timers.current.delete(issue.key)
      nudged.current.delete(issue.key)
      void persist(issue.key, startDate, dueDate)
    }
    if (debounce) timers.current.set(issue.key, { timer: setTimeout(run, 500), run })
    else run()
  }
  const latest = (issue: Issue) => qc.getQueryData<SearchResult>(issuesKey)?.issues.find((x) => x.id === issue.id) ?? issue

  const schedule = (issue: Issue, from?: number, rollup?: [number, number] | null) => {
    if (from == null && rollup) return void setDates(issue, rollup)
    const v = viewDays()
    const a = from ?? (today >= v.first && today <= v.last - 3 ? today : Math.round(v.first + 2))
    const len = zoom === 'day' ? 3 : zoom === 'week' ? 6 : 13
    void setDates(issue, [a, a + len])
  }

  // ---- pointer dragging ----
  const onPointerDown = (e: ReactPointerEvent<HTMLElement>, issue: Issue) => {
    if (!canEdit || e.button !== 0) return
    const handle = (e.target as HTMLElement).dataset.handle
    dragRef.current = { issue, mode: handle === 'start' || handle === 'end' ? handle : 'move', x0: e.clientX, moved: false }
    e.currentTarget.setPointerCapture(e.pointerId)
  }
  const onPointerMove = (e: ReactPointerEvent<HTMLElement>) => {
    const r = dragRef.current
    if (!r) return
    const dx = e.clientX - r.x0
    if (!r.moved && Math.abs(dx) < 4) return
    r.moved = true
    const delta = Math.round(dx / ppd)
    setDrag((d) => (d && d.key === r.issue.key && d.delta === delta && d.mode === r.mode ? d : { key: r.issue.key, mode: r.mode, delta }))
  }
  const onPointerUp = (e: ReactPointerEvent<HTMLElement>) => {
    const r = dragRef.current
    dragRef.current = null
    if (!r) return
    if (!r.moved) {
      open(r.issue.key)
      return
    }
    const delta = Math.round((e.clientX - r.x0) / ppd)
    if (!delta) {
      setDrag(null)
      return
    }
    void setDates(r.issue, shifted(spanOfIssue(r.issue), r.mode, delta))
  }
  const onPointerCancel = () => {
    dragRef.current = null
    setDrag(null)
  }

  // ---- keyboard ----
  const focusSibling = (el: HTMLElement, dir: number) => {
    const all = [...(scroller.current?.querySelectorAll<HTMLElement>('[data-tl-bar]') ?? [])]
    all[all.indexOf(el) + dir]?.focus()
  }
  const onBarKey = (e: ReactKeyboardEvent<HTMLElement>, issue: Issue) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault()
      open(issue.key)
    } else if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
      e.preventDefault()
      focusSibling(e.currentTarget, e.key === 'ArrowUp' ? -1 : 1)
    } else if (canEdit && (e.key === 'ArrowLeft' || e.key === 'ArrowRight')) {
      e.preventDefault()
      const cur = latest(issue)
      const mode: DragMode = e.shiftKey ? 'end' : e.altKey ? 'start' : 'move'
      const span = shifted(nudged.current.get(issue.key) ?? spanOfIssue(cur), mode, e.key === 'ArrowLeft' ? -1 : 1)
      nudged.current.set(issue.key, span)
      void setDates(cur, span, true)
    }
  }
  const reveal = (key: string) => {
    const g = groups.find((x) => x.items.some((r) => r.kind === 'issue' && r.issue.key === key))
    if (g && collapsed.has(g.id)) toggle(g.id)
    requestAnimationFrame(() => {
      const el = scroller.current?.querySelector<HTMLElement>(`[data-tl-bar="${CSS.escape(key)}"]`)
      if (el) {
        el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'smooth' })
        el.focus({ preventScroll: true })
      } else open(key)
    })
  }

  // ---- geometry for the current (possibly dragged) dates ----
  const spanFor = (i: Issue): Span => (drag && drag.key === i.key ? shifted(spanOfIssue(i), drag.mode, drag.delta) : spanOfIssue(i))
  const rowOf = new Map<string, number>()
  rows.forEach((r, k) => r.kind === 'issue' && rowOf.set(r.issue.key, k))
  const arrows = deps.flatMap((dep) => {
    const ia = rowOf.get(dep.from)
    const ib = rowOf.get(dep.to)
    if (ia == null || ib == null) return []
    const a = (rows[ia] as Extract<Row, { kind: 'issue' }>).issue
    const b = (rows[ib] as Extract<Row, { kind: 'issue' }>).issue
    const sa = spanFor(a)
    const sb = spanFor(b)
    const ga = geometry(sa, range.start, ppd)
    const gb = geometry(sb, range.start, ppd)
    if (!ga || !gb) return []
    return [{ id: `${dep.from}>${dep.to}`, d: arrowPath(edges(ga)[1], ia * ROW_H + ROW_H / 2, edges(gb)[0], ib * ROW_H + ROW_H / 2), bad: isConflict(a, b, sa, sb), title: t('{a} blocks {b}', { a: dep.from, b: dep.to }) }]
  })

  // ---- scale ----
  const fmt = useMemo(() => {
    const l = locale()
    return {
      monthYear: new Intl.DateTimeFormat(l, { month: 'long', year: 'numeric', timeZone: 'UTC' }),
      month: new Intl.DateTimeFormat(l, { month: 'short', timeZone: 'UTC' }),
      weekday: new Intl.DateTimeFormat(l, { weekday: 'narrow', timeZone: 'UTC' }),
      full: new Intl.DateTimeFormat(l, { weekday: 'long', day: 'numeric', month: 'long', year: 'numeric', timeZone: 'UTC' }),
    }
  }, [])
  const scale = useMemo(() => {
    type Cell = { key: string; x: number; w: number; label: string; title?: string; today?: boolean; weekend?: boolean }
    const top: Cell[] = []
    const bottom: Cell[] = []
    const cell = (a: number, b: number, label: string, extra?: Partial<Cell>): Cell => ({ key: `${a}`, x: (a - range.start) * ppd, w: (b - a) * ppd, label, ...extra })
    if (zoom === 'month') {
      for (let a = range.start; a < end; ) {
        const y = utc(a).getUTCFullYear()
        const b = Math.min(end, Math.round(Date.UTC(y + 1, 0, 1) / DAY_MS))
        top.push(cell(a, b, String(y)))
        a = b
      }
      for (let a = range.start; a < end; ) {
        const b = Math.min(end, nextMonth(a))
        bottom.push(cell(a, b, fmt.month.format(utc(a)), { title: fmt.monthYear.format(utc(a)), today: today >= a && today < b }))
        a = b
      }
    } else {
      for (let a = range.start; a < end; ) {
        const b = Math.min(end, nextMonth(a))
        top.push(cell(a, b, fmt.monthYear.format(utc(a))))
        a = b
      }
      if (zoom === 'day') {
        for (let a = range.start; a < end; a++) {
          const wd = weekday(a)
          bottom.push(cell(a, a + 1, `${fmt.weekday.format(utc(a))} ${utc(a).getUTCDate()}`, { title: fmt.full.format(utc(a)), today: a === today, weekend: wd === 0 || wd === 6 }))
        }
      } else {
        for (let a = range.start; a < end; a += 7)
          bottom.push(cell(a, a + 7, `${utc(a).getUTCDate()}–${utc(a + 6).getUTCDate()}`, { title: `${shortDate(diso(a))} – ${shortDate(diso(a + 6))}`, today: today >= a && today < a + 7 }))
      }
    }
    return { top, bottom }
  }, [range.start, end, zoom, ppd, fmt, today])
  const sprints = (project.kind === 'scrum' ? project.sprints : [])
    .filter((s) => s.startAt && s.endAt)
    .map((s) => {
      const a = dnum(isoDate(new Date(s.startAt!)))
      const b = dnum(isoDate(new Date(s.endAt!)))
      return { s, x: (a - range.start) * ppd, w: (b - a + 1) * ppd }
    })
    .filter((b) => b.x + b.w > 0 && b.x < W)
  const todayX = today >= range.start && today < end ? (today - range.start + 0.5) * ppd : null
  const firstSat = mod(6 - weekday(range.start), 7)
  const headH = 24 + 26 + (sprints.length ? 22 : 0)
  const epicType = project.types.find((x) => x.level === 1)

  // ---- rendering helpers ----
  const progressOf = (i: Issue): number | null => {
    if (i.childCount > 0) return i.childDone / i.childCount
    return i.status.category === 'done' ? 1 : null
  }
  const renderTrack = (row: Extract<Row, { kind: 'issue' }>) => {
    const i = row.issue
    const span = spanFor(i)
    const g = geometry(span, range.start, ppd)
    const isEpic = i.type.level >= 1
    const dragging = drag?.key === i.key
    if (!g) {
      return (
        <>
          {isEpic && row.rollup && (
            <div
              className="tl-rollup"
              style={{ left: (row.rollup[0] - range.start) * ppd, width: (row.rollup[1] - row.rollup[0] + 1) * ppd, '--epic': i.type.color } as CSSProperties}
              title={t('Dates of its issues: {dates}', { dates: dateText(row.rollup) })}
            />
          )}
          <div className="tl-nodates">
            <span>{t('No dates')}</span>
            {canEdit && i.status.category !== 'done' && (
              <button
                className="btn btn-ghost btn-sm"
                onClick={(e) => {
                  e.stopPropagation()
                  schedule(i, undefined, isEpic ? row.rollup : null)
                }}
                title={isEpic && row.rollup ? t('Use the dates of its issues') : t('Give it a start and due date')}
              >
                <CalendarPlus size={13} /> {t('Schedule')}
              </button>
            )}
          </div>
        </>
      )
    }
    const progress = progressOf(i)
    const label = dragging ? dateText(span) : i.title
    const desc = `${i.key} ${i.title}\n${i.status.name} · ${dateText(span)}${i.childCount ? `\n${t('{done} of {count} done', { done: i.childDone, count: i.childCount })}` : ''}`
    const common = {
      'data-tl-bar': i.key,
      tabIndex: 0,
      role: 'button',
      'aria-label': `${i.key} ${i.title}: ${dateText(span)}`,
      'aria-describedby': `${uid}-keys`,
      title: dragging ? undefined : desc,
      onPointerDown: (e: ReactPointerEvent<HTMLElement>) => onPointerDown(e, i),
      onPointerMove,
      onPointerUp,
      onPointerCancel,
      onClick: () => !canEdit && open(i.key),
      onKeyDown: (e: ReactKeyboardEvent<HTMLElement>) => onBarKey(e, i),
    }
    const cls = `tl-${i.status.category} ${isEpic ? 'tl-epic' : ''} ${conflictKeys.has(i.key) ? 'tl-has-conflict' : ''} ${dragging ? 'tl-dragging' : ''} ${row.context ? 'tl-context' : ''} ${canEdit ? 'tl-editable' : ''}`
    if (g.kind === 'diamond') {
      return (
        <>
          <div className={`tl-diamond ${cls}`} style={{ left: g.x - 9, '--epic': i.type.color } as CSSProperties} {...common}>
            <i />
          </div>
          <span className="tl-bar-label tl-outside" style={{ left: g.x + 14 }}>{label}</span>
        </>
      )
    }
    const need = (label.length + (isEpic && i.childCount ? 8 : 0)) * 6.6 + 18
    const inside = !dragging && (g.w >= need || g.w >= 160)
    return (
      <>
        {isEpic && row.rollup && (row.rollup[0] < Math.min(...(span.filter((x) => x != null) as number[])) || row.rollup[1] > Math.max(...(span.filter((x) => x != null) as number[]))) && (
          <div
            className="tl-rollup-line"
            style={{ left: (row.rollup[0] - range.start) * ppd, width: (row.rollup[1] - row.rollup[0] + 1) * ppd }}
            title={t('Its issues run {dates}', { dates: dateText(row.rollup) })}
          />
        )}
        <div className={`tl-bar ${cls} ${g.open ? 'tl-open' : ''}`} style={{ left: g.x, width: g.w, '--epic': i.type.color } as CSSProperties} {...common}>
          {progress != null && <span className="tl-progress" style={{ width: `${Math.round(progress * 100)}%` }} />}
          {inside && (
            <span className="tl-bar-label">
              {label}
              {isEpic && i.childCount > 0 && <span className="tl-bar-count"> · {i.childDone}/{i.childCount}</span>}
            </span>
          )}
          {canEdit && (
            <>
              <span className="tl-handle tl-handle-start" data-handle="start" />
              <span className="tl-handle tl-handle-end" data-handle="end" />
            </>
          )}
        </div>
        {!inside && <span className={`tl-bar-label tl-outside ${dragging ? 'tl-drag-tip' : ''}`} style={{ left: g.x + g.w + 6 }}>{label}</span>}
      </>
    )
  }

  if (issues.length === 0) {
    return (
      <div className="tl-wrap">
        <Head project={project} canEdit={canEdit} onCreate={() => create({ projectKey: project.key, typeId: epicType?.id })} epic={!!epicType} />
        <Empty icon={<ChartGantt size={36} />} title={t('Nothing to plan yet')}>
          <p>{t('Create epics and issues and give them start and due dates; they show up here as bars you can drag.')}</p>
        </Empty>
      </div>
    )
  }

  return (
    <div className={`tl-wrap ${compact ? 'tl-compact' : ''} ${drag ? 'tl-dragging-active' : ''}`}>
      <Head project={project} canEdit={canEdit} onCreate={() => create({ projectKey: project.key, typeId: epicType?.id })} epic={!!epicType} />
      <div className="toolbar tl-toolbar">
        <div className="row tl-search">
          <Search size={14} className="faint" aria-hidden />
          <input className="input input-sm" placeholder={t('Filter issues…')} value={text} onChange={(e) => setText(e.target.value)} aria-label={t('Filter issues')} />
        </div>
        {meId && (
          <button className={`chip ${mine ? 'on' : ''}`} aria-pressed={mine} onClick={() => setMine((m) => !m)}>
            {t('Only my issues')}
          </button>
        )}
        <button className={`chip ${showDone ? 'on' : ''}`} aria-pressed={showDone} onClick={() => setPrefs({ showDone: !showDone })}>
          {t('Show done')}
        </button>
        <span className="spacer" />
        {links.isFetching && <Spinner size={14} />}
        <button className="btn btn-sm" onClick={goToday}>
          <Crosshair size={14} /> {t('Today')}
        </button>
        <div className="seg" role="group" aria-label={t('Zoom')}>
          {(['day', 'week', 'month'] as Zoom[]).map((z) => (
            <button key={z} className={zoom === z ? 'active' : ''} aria-pressed={zoom === z} onClick={() => changeZoom(z)}>
              {zoomLabel(z)}
            </button>
          ))}
        </div>
        <PopButton className="icon-btn" title={t('Keyboard shortcuts')} label={<Keyboard size={16} />} width={300} align="end">
          {() => <KeyHelp canEdit={canEdit} />}
        </PopButton>
      </div>
      <p id={`${uid}-keys`} className="sr-only">
        {canEdit ? t('Enter opens the issue. Left and right arrows move it by a day; with Shift they change the due date, with Alt the start date. Up and down arrows go to the next bar.') : t('Enter opens the issue. Up and down arrows go to the next bar.')}
      </p>

      {conflicts.length > 0 && (
        <div className="tl-banner" role="status">
          <TriangleExclamationPoint size={15} aria-hidden />
          <strong>{t('Scheduling conflicts')}</strong>
          <span className="tl-banner-list">
            {conflicts.slice(0, 4).map((c) => (
              <span key={`${c.from}>${c.to}`} className="tl-banner-item">
                {t('{blocked} starts before {blocker} (which blocks it) is due', { blocked: '\u0000', blocker: '\u0001' })
                  .split(/(\u0000|\u0001)/)
                  .map((part, k) =>
                    part === '\u0000' ? (
                      <button key={k} className="tl-keybtn" onClick={() => reveal(c.to)}>{c.to}</button>
                    ) : part === '\u0001' ? (
                      <button key={k} className="tl-keybtn" onClick={() => reveal(c.from)}>{c.from}</button>
                    ) : (
                      part
                    ),
                  )}
              </span>
            ))}
            {conflicts.length > 4 && <span className="faint">{t('and {n} more', { n: conflicts.length - 4 })}</span>}
          </span>
        </div>
      )}

      <div className="tl-scroller" ref={scroller}>
        <div className="tl-canvas" style={{ width: `calc(var(--tl-left) + ${W}px)` }}>
          <div className="tl-header" style={{ height: headH }}>
            <div className="tl-corner" ref={corner}>
              <span className="tl-corner-title">{t('Issues')}</span>
              <span className="spacer" />
              <button
                className="icon-btn icon-btn-sm"
                onClick={() => setCollapsed(allCollapsed ? new Set() : new Set(groups.map((g) => g.id)))}
                title={allCollapsed ? t('Expand all') : t('Collapse all')}
                aria-label={allCollapsed ? t('Expand all') : t('Collapse all')}
              >
                {allCollapsed ? <ChevronsUpDown size={14} /> : <ChevronsDownUp size={14} />}
              </button>
              <button className="icon-btn icon-btn-sm tl-compact-btn" onClick={() => setPrefs({ compact: !compact })} title={compact ? t('Show titles') : t('Hide titles')} aria-label={compact ? t('Show titles') : t('Hide titles')} aria-pressed={compact}>
                {compact ? <PanelLeftOpen size={14} /> : <PanelLeftClose size={14} />}
              </button>
            </div>
            <div className="tl-scale" style={{ width: W }}>
              <div className="tl-tier tl-tier-top">
                {scale.top.map((c) => (
                  <div key={c.key} className="tl-cell" style={{ left: c.x, width: c.w }}>
                    <span>{c.label}</span>
                  </div>
                ))}
              </div>
              <div className={`tl-tier tl-tier-bottom tl-z-${zoom}`}>
                {scale.bottom.map((c) => (
                  <div key={c.key} className={`tl-cell ${c.today ? 'tl-is-today' : ''} ${c.weekend ? 'tl-is-weekend' : ''}`} style={{ left: c.x, width: c.w }} title={c.title}>
                    {c.label}
                  </div>
                ))}
              </div>
              {sprints.length > 0 && (
                <div className="tl-tier tl-tier-sprints">
                  {sprints.map(({ s, x, w }) => (
                    <div key={s.id} className={`tl-sprint tl-sprint-${s.state}`} style={{ left: x, width: w }} title={`${s.name}\n${shortDate(s.startAt)} – ${shortDate(s.endAt)}${s.goal ? `\n${s.goal}` : ''}`}>
                      <span>{s.name}</span>
                    </div>
                  ))}
                </div>
              )}
              {todayX != null && <div className="tl-today-mark" style={{ left: todayX }} title={t('Today')} />}
            </div>
          </div>

          <div className="tl-body" style={{ minHeight: rows.length * ROW_H }}>
            <div
              className="tl-bg"
              style={
                zoom === 'month'
                  ? { width: W }
                  : { width: W, backgroundImage: `linear-gradient(to right, var(--tl-weekend) 0 ${2 * ppd}px, transparent ${2 * ppd}px)`, backgroundSize: `${7 * ppd}px 100%`, backgroundPosition: `${firstSat * ppd}px 0` }
              }
              aria-hidden
            >
              {sprints.filter(({ s }) => s.state === 'active').map(({ s, x, w }) => <div key={s.id} className="tl-sprint-shade" style={{ left: x, width: w }} />)}
              {scale.bottom.map((c) => <i key={c.key} className="tl-gridline" style={{ left: c.x }} />)}
              {todayX != null && <div className="tl-today" style={{ left: todayX }} />}
            </div>

            {rows.length === 0 && (
              <div className="tl-row tl-empty-row">
                <div className="tl-label faint">{t('No issues match the filters')}</div>
              </div>
            )}

            {rows.map((row) => {
              if (row.kind === 'group') {
                const isCollapsed = collapsed.has(row.id)
                return (
                  <div key={`g-${row.id}`} className="tl-row tl-group-row">
                    <div className="tl-label">
                      <button className="icon-btn icon-btn-sm tl-toggle" onClick={() => toggle(row.id)} aria-expanded={!isCollapsed} aria-label={isCollapsed ? t('Expand') : t('Collapse')}>
                        {isCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
                      </button>
                      {row.color && <span className="dot" style={{ background: row.color }} />}
                      {row.parentKey ? (
                        <button className="tl-name" onClick={() => open(row.parentKey!)} title={`${row.parentKey} ${row.label}`}>
                          <span className="issue-key">{row.parentKey}</span>
                          <span className="tl-title" dir="auto">{row.label}</span>
                        </button>
                      ) : (
                        <span className="tl-name tl-group-title"><span className="tl-group-text">{row.label}</span></span>
                      )}
                      <span className="tl-count">{row.count}</span>
                    </div>
                    <div className="tl-track" style={{ width: W }}>
                      {row.rollup && (
                        <div className="tl-rollup" style={{ left: (row.rollup[0] - range.start) * ppd, width: (row.rollup[1] - row.rollup[0] + 1) * ppd, '--epic': row.color ?? 'var(--text-faint)' } as CSSProperties} title={dateText(row.rollup)} />
                      )}
                    </div>
                  </div>
                )
              }
              const i = row.issue
              const isHead = row.group != null
              const isCollapsed = isHead && collapsed.has(row.group!)
              const undated = !i.startDate && !i.dueDate && i.status.category !== 'done'
              return (
                <div key={i.id} className={`tl-row ${isHead ? 'tl-group-row tl-epic-row' : ''} ${row.context ? 'tl-context-row' : ''}`}>
                  <div className="tl-label" style={{ '--depth': row.depth } as CSSProperties}>
                    {isHead ? (
                      <button className="icon-btn icon-btn-sm tl-toggle" onClick={() => toggle(row.group!)} aria-expanded={!isCollapsed} aria-label={isCollapsed ? t('Expand') : t('Collapse')} disabled={!row.count}>
                        {isCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
                      </button>
                    ) : (
                      <span className="tl-indent" />
                    )}
                    <TypeIcon icon={i.type.icon} color={i.type.color} size={16} title={i.type.name} />
                    <button className="tl-name" onClick={() => open(i.key)} title={`${i.key} ${i.title}`}>
                      <span className={`issue-key ${i.status.category === 'done' ? 'tl-done-key' : ''}`}>{i.key}</span>
                      <span className="tl-title" dir="auto">{i.title}</span>
                    </button>
                    {isHead && i.childCount > 0 && <span className="tl-count" title={t('{done} of {count} done', { done: i.childDone, count: i.childCount })}>{i.childDone}/{i.childCount}</span>}
                  </div>
                  <div
                    className={`tl-track ${undated && canEdit ? 'tl-schedulable' : ''}`}
                    style={{ width: W }}
                    title={undated && canEdit ? t('Click a day to schedule it from there') : undefined}
                    onClick={
                      undated && canEdit
                        ? (e) => {
                            if ((e.target as HTMLElement).closest('button, .tl-nodates')) return
                            const r = e.currentTarget.getBoundingClientRect()
                            schedule(i, range.start + Math.floor((e.clientX - r.left) / ppd))
                          }
                        : undefined
                    }
                  >
                    {renderTrack(row)}
                  </div>
                </div>
              )
            })}
            <div className="tl-row tl-filler" aria-hidden>
              <div className="tl-label" />
            </div>

            <svg className="tl-links" width={W} height={rows.length * ROW_H} aria-hidden>
              <defs>
                <marker id={`${uid}-ok`} viewBox="0 0 8 8" refX="7.5" refY="4" markerWidth="5" markerHeight="5" orient="auto">
                  <path d="M0,0 L8,4 L0,8 z" className="tl-head-ok" />
                </marker>
                <marker id={`${uid}-bad`} viewBox="0 0 8 8" refX="7.5" refY="4" markerWidth="4.5" markerHeight="4.5" orient="auto">
                  <path d="M0,0 L8,4 L0,8 z" className="tl-head-bad" />
                </marker>
              </defs>
              {arrows.map((a) => (
                <path key={a.id} d={a.d} className={`tl-link ${a.bad ? 'tl-bad' : ''}`} markerEnd={`url(#${uid}-${a.bad ? 'bad' : 'ok'})`}>
                  <title>{a.title}</title>
                </path>
              ))}
            </svg>
          </div>
        </div>
      </div>
    </div>
  )
}

function Head({ project, canEdit, onCreate, epic }: { project: Project; canEdit: boolean; onCreate: () => void; epic: boolean }) {
  return (
    <div className="tl-head">
      <div className="crumbs">
        <Link to="/projects">{t('Projects')}</Link>
        <span>/</span>
        <Link to={`/projects/${project.key}`}>{project.name}</Link>
      </div>
      <div className="page-head" style={{ marginBottom: 10 }}>
        <h1>{t('Timeline')}</h1>
        <span className="spacer" />
        {canEdit && (
          <button className="btn" onClick={onCreate}>
            <Plus size={14} /> {epic ? t('Create epic') : t('Create issue')}
          </button>
        )}
      </div>
    </div>
  )
}

function KeyHelp({ canEdit }: { canEdit: boolean }) {
  const rows: [string, string][] = [
    [t('Tab [[key]]'), t('Go to the next bar')],
    ['↑ ↓', t('Previous / next bar')],
    [t('Enter [[key]]'), t('Open the issue')],
  ]
  if (canEdit)
    rows.push(
      ['← →', t('Move by a day')],
      [`${t('Shift [[key]]')} + ← →`, t('Change the due date')],
      [`${t('Alt [[key]]')} + ← →`, t('Change the start date')],
    )
  return (
    <div className="tl-keys">
      <div className="tl-keys-title">{t('Keyboard shortcuts')}</div>
      {rows.map(([k, v]) => (
        <div key={k} className="tl-keys-row">
          <span className="kbd">{k}</span>
          <span>{v}</span>
        </div>
      ))}
      {canEdit && <p className="faint small">{t('Drag a bar to move it, or its ends to change the start or due date.')}</p>}
    </div>
  )
}
