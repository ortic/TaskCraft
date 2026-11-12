// Hand-rolled SVG charts (no chart library): a line/step chart, grouped columns, stacked areas,
// a scatter and an HTML bar list, plus the legend, card and table-view pieces around them.
// Every plot fills its container's width (ResizeObserver), is a single tab stop whose arrow keys
// walk the data (showing the same tooltip as hovering), carries a role="img" summary, and takes
// its colours only from CSS variables (`--rp-*`, defined in pages/Reports.css for the light, dark
// and high-contrast themes).

import { useId, useLayoutEffect, useState, type CSSProperties, type KeyboardEvent, type PointerEvent, type ReactNode } from 'react'
import { ChartColumn, Table2 } from 'lucide-react'
import { isoDate, shortDate } from '../api'
import { locale, t } from '../i18n'

export const DAY = 86_400_000

/** Categorical slots, assigned in this order and never cycled. */
export const SERIES = ['var(--rp-s1)', 'var(--rp-s2)', 'var(--rp-s3)', 'var(--rp-s4)', 'var(--rp-s5)', 'var(--rp-s6)', 'var(--rp-s7)', 'var(--rp-s8)']
/** Mark colours for the three status categories. */
export const CATEGORY_COLOR = { todo: 'var(--rp-todo)', in_progress: 'var(--rp-progress)', done: 'var(--rp-done)' } as const
/** De-emphasised context series (e.g. "committed" next to "completed") and folded "Other". */
export const MUTED = 'var(--rp-muted)'
/** Reference lines: ideal, average, median. */
export const REF = 'var(--rp-ref)'

// ---------- numbers, scales, ticks ----------

export function fmt(v: number, digits = 1): string {
  // round first so float noise just below zero doesn't print as "-0"
  const k = 10 ** digits
  return new Intl.NumberFormat(locale(), { maximumFractionDigits: digits }).format(Math.round(v * k) / k || 0)
}

export function pct(part: number, whole: number): string {
  return new Intl.NumberFormat(locale(), { style: 'percent', maximumFractionDigits: 0 }).format(whole ? part / whole : 0)
}

function tickText(v: number, compact: boolean): string {
  return new Intl.NumberFormat(locale(), compact ? { notation: 'compact', maximumFractionDigits: 1 } : { maximumFractionDigits: 2 }).format(v)
}

type Scale = (v: number) => number

function linear(d0: number, d1: number, r0: number, r1: number): Scale {
  const k = d1 === d0 ? 0 : (r1 - r0) / (d1 - d0)
  return (v) => r0 + (v - d0) * k
}

const r1 = (v: number) => Math.round(v * 10) / 10
const crisp = (v: number) => Math.round(v) + 0.5

/** Round tick values covering [min, max]. */
export function niceTicks(min: number, max: number, count = 5, integer = false): number[] {
  if (!Number.isFinite(min) || !Number.isFinite(max)) {
    min = 0
    max = 1
  }
  if (max <= min) max = min + (integer ? 4 : 1)
  const raw = (max - min) / Math.max(1, count)
  let step = 10 ** Math.floor(Math.log10(raw))
  const err = raw / step
  if (err >= 7.5) step *= 10
  else if (err >= 3.5) step *= 5
  else if (err >= 1.5) step *= 2
  if (integer) step = Math.max(1, Math.round(step))
  const first = Math.floor(min / step + 1e-9)
  const last = Math.ceil(max / step - 1e-9)
  const out: number[] = []
  for (let i = first; i <= last; i++) out.push(Number((i * step).toPrecision(12)))
  return out
}

function startOfDay(ms: number): number {
  const d = new Date(ms)
  d.setHours(0, 0, 0, 0)
  return d.getTime()
}

/** Local-midnight timestamps between t0 and t1, at most `max` of them. */
function timeTicks(t0: number, t1: number, max: number): number[] {
  const days = (t1 - t0) / DAY
  const step = [1, 2, 3, 7, 14, 28, 56, 91, 182, 364].find((s) => days / s <= Math.max(1, max)) ?? 364
  const out: number[] = []
  const d = new Date(startOfDay(t0))
  if (d.getTime() < t0) d.setDate(d.getDate() + 1)
  while (d.getTime() <= t1 + 1000 && out.length < 400) {
    out.push(d.getTime())
    d.setDate(d.getDate() + step)
  }
  return out
}

/** `12 Nov` for a timestamp (local day). */
export function dayLabel(ms: number): string {
  return shortDate(isoDate(new Date(ms)))
}

/** Local-midnight timestamp of a `YYYY-MM-DD` date. */
export function dayMs(date: string): number {
  return new Date(date + 'T00:00:00').getTime()
}

const CHAR_W = 6.4

function yAxis(ticks: number[]) {
  const compact = Math.max(...ticks.map(Math.abs)) >= 10000
  const labels = ticks.map((v) => tickText(v, compact))
  return { labels, width: Math.max(26, Math.max(...labels.map((s) => s.length)) * 7 + 12) }
}

function nearest(positions: number[], px: number): number | null {
  let best: number | null = null
  let dist = Infinity
  positions.forEach((p, i) => {
    const d = Math.abs(p - px)
    if (d < dist) {
      dist = d
      best = i
    }
  })
  return best
}

// ---------- the plot frame: sizing, pointer + keyboard, tooltip ----------

interface Tip {
  x: number
  y: number
  content: ReactNode
  /** horizontal distance between the anchor and the tooltip (px) */
  offset?: number
}

interface Built {
  /** how many positions the pointer and the keyboard can inspect */
  count: number
  /** the position under the pointer (plot-local px), or null */
  locate: (x: number, y: number) => number | null
  draw: (active: number | null) => ReactNode
  tip: (i: number) => Tip | null
  /** where the keyboard starts (default: the last position) */
  start?: number
  /** whether a click at (x, y) means position i (default: any click on the plot) */
  near?: (i: number, x: number, y: number) => boolean
}

function useWidth(el: HTMLElement | null): number {
  const [w, setW] = useState(0)
  useLayoutEffect(() => {
    if (!el) return
    setW(Math.floor(el.getBoundingClientRect().width))
    const ro = new ResizeObserver((entries) => setW(Math.floor(entries[0].contentRect.width)))
    ro.observe(el)
    return () => ro.disconnect()
  }, [el])
  return w
}

function Plot({ summary, height, build, onActivate, hint }: { summary: string; height: number; build: (width: number) => Built; onActivate?: (i: number) => void; hint?: string }) {
  const [el, setEl] = useState<HTMLDivElement | null>(null)
  const width = useWidth(el)
  const [active, setActive] = useState<number | null>(null)
  const [near, setNear] = useState(false)
  const b = width > 0 ? build(width) : null
  const cur = b && active !== null && active < b.count ? active : null
  const tip = b && cur !== null ? b.tip(cur) : null

  const point = (e: PointerEvent<HTMLDivElement>) => {
    if (!b) return
    const r = e.currentTarget.getBoundingClientRect()
    const x = e.clientX - r.left
    const y = e.clientY - r.top
    const i = b.locate(x, y)
    setActive(i)
    setNear(i !== null && (!b.near || b.near(i, x, y)))
  }
  const key = (e: KeyboardEvent<HTMLDivElement>) => {
    if (!b || !b.count) return
    const last = b.count - 1
    let next: number
    switch (e.key) {
      case 'ArrowRight':
      case 'ArrowDown':
        next = cur === null ? (b.start ?? last) : Math.min(last, cur + 1)
        break
      case 'ArrowLeft':
      case 'ArrowUp':
        next = cur === null ? (b.start ?? last) : Math.max(0, cur - 1)
        break
      case 'Home':
        next = 0
        break
      case 'End':
        next = last
        break
      case 'Escape':
        setActive(null)
        return
      case 'Enter':
      case ' ':
        if (cur !== null && onActivate) {
          e.preventDefault()
          onActivate(cur)
        }
        return
      default:
        return
    }
    e.preventDefault()
    setActive(next)
  }

  return (
    <div
      ref={setEl}
      className="rp-plot rp-viz"
      style={{ height }}
      tabIndex={0}
      role="group"
      aria-label={`${summary} ${hint ?? t('Use the arrow keys to read the values.')}`}
      data-clickable={onActivate && cur !== null && near ? '' : undefined}
      onPointerMove={point}
      onPointerDown={point}
      onPointerLeave={(e) => e.pointerType === 'mouse' && setActive(null)}
      onFocus={(e) => {
        if (b?.count && e.currentTarget.matches(':focus-visible')) setActive((a) => a ?? b.start ?? b.count - 1)
      }}
      onBlur={() => setActive(null)}
      onKeyDown={key}
      onClick={() => cur !== null && near && onActivate?.(cur)}
    >
      {b && (
        <svg className="rp-svg" width={width} height={height} viewBox={`0 0 ${width} ${height}`} role="img" aria-label={summary}>
          {b.draw(cur)}
        </svg>
      )}
      {tip && (
        <div
          className={`rp-tip ${tip.x > width / 2 ? 'rp-tip-left' : ''}`}
          style={{ left: tip.x, top: tip.y, '--off': `${tip.offset ?? 12}px` } as CSSProperties}
          role="status"
        >
          {tip.content}
        </div>
      )}
    </div>
  )
}

// ---------- shared svg pieces ----------

function Grid({ ticks, labels, y, x0, x1 }: { ticks: number[]; labels: string[]; y: Scale; x0: number; x1: number }) {
  return (
    <g aria-hidden>
      {ticks.map((v, i) => (
        <g key={i}>
          <line x1={x0} x2={x1} y1={crisp(y(v))} y2={crisp(y(v))} className={v === 0 ? 'rp-baseline' : 'rp-gridline'} />
          <text x={x0 - 8} y={y(v)} dy="0.32em" textAnchor="end" className="rp-tick">
            {labels[i]}
          </text>
        </g>
      ))}
    </g>
  )
}

function XTicks({ items, y, width }: { items: { x: number; label: string }[]; y: number; width: number }) {
  return (
    <g aria-hidden>
      {items.map((it, i) => {
        const half = (it.label.length * CHAR_W) / 2
        const anchor = it.x - half < 0 ? 'start' : it.x + half > width ? 'end' : 'middle'
        return (
          <text key={i} x={anchor === 'start' ? 0 : anchor === 'end' ? width : it.x} y={y} textAnchor={anchor} className="rp-tick">
            {it.label}
          </text>
        )
      })}
    </g>
  )
}

export interface RefLine {
  value: number
  label: string
}

/** Dashed horizontal reference lines (average, median…) labelled at the right end; lines too
 *  close to label apart share one label above the topmost of them. */
function RefLines({ refs, y, x0, x1 }: { refs: RefLine[]; y: Scale; x0: number; x1: number }) {
  const sorted = [...refs].sort((a, b) => b.value - a.value)
  const groups: RefLine[][] = []
  for (const r of sorted) {
    const g = groups[groups.length - 1]
    if (g && y(r.value) - y(g[g.length - 1].value) < 15) g.push(r)
    else groups.push([r])
  }
  return (
    <g aria-hidden>
      {sorted.map((r, i) => (
        <line key={i} x1={x0} x2={x1} y1={crisp(y(r.value))} y2={crisp(y(r.value))} className="rp-refline" />
      ))}
      {groups.map((g, i) => (
        <text key={i} x={x1} y={y(g[0].value) - 5} textAnchor="end" className="rp-reflabel">
          {g.map((r) => r.label).join(' · ')}
        </text>
      ))}
    </g>
  )
}

/** Column with a 4px rounded data end and a square baseline. */
function colPath(x: number, y: number, w: number, h: number): string {
  if (h <= 0 || w <= 0) return ''
  const r = Math.min(4, w / 2, h)
  return `M${r1(x)},${r1(y + h)}V${r1(y + r)}Q${r1(x)},${r1(y)} ${r1(x + r)},${r1(y)}H${r1(x + w - r)}Q${r1(x + w)},${r1(y)} ${r1(x + w)},${r1(y + r)}V${r1(y + h)}Z`
}

function pathOf(pts: [number, number][], step: boolean): string {
  if (!pts.length) return ''
  let d = `M${r1(pts[0][0])},${r1(pts[0][1])}`
  for (let i = 1; i < pts.length; i++) d += step ? `H${r1(pts[i][0])}V${r1(pts[i][1])}` : `L${r1(pts[i][0])},${r1(pts[i][1])}`
  return d
}

// ---------- tooltip content ----------

/** Tooltip body: a title and rows with the value first and the series name after it. */
export function TipBody({ title, rows, note }: { title: ReactNode; rows: { color?: string; kind?: KeyKind; value: ReactNode; label: ReactNode }[]; note?: ReactNode }) {
  return (
    <>
      <div className="rp-tip-title">{title}</div>
      {rows.map((r, i) => (
        <div key={i} className="rp-tip-row">
          {r.color ? <Key color={r.color} kind={r.kind ?? 'line'} /> : <span className="rp-key rp-key-none" />}
          <strong>{r.value}</strong>
          <span className="rp-tip-label">{r.label}</span>
        </div>
      ))}
      {note && <div className="rp-tip-note">{note}</div>}
    </>
  )
}

// ---------- legend, card, table view ----------

export type KeyKind = 'rect' | 'line' | 'dash' | 'dot' | 'ring' | 'none'

function Key({ color, kind }: { color: string; kind: KeyKind }) {
  return <span className={`rp-key rp-key-${kind}`} style={{ '--c': color } as CSSProperties} aria-hidden />
}

export interface LegendItem {
  label: string
  color: string
  kind?: KeyKind
}

export function Legend({ items }: { items: LegendItem[] }) {
  return (
    <ul className="rp-legend rp-viz">
      {items.map((i) => (
        <li key={i.label}>
          <Key color={i.color} kind={i.kind ?? 'rect'} />
          {i.label}
        </li>
      ))}
    </ul>
  )
}

export interface TableData {
  columns: string[]
  rows: ReactNode[][]
  /** columns holding numbers (right-aligned, tabular figures) */
  numeric?: number[]
}

export function DataTable({ columns, rows, numeric = [] }: TableData) {
  return (
    <div className="rp-table-wrap">
      <table className="list rp-table">
        <thead>
          <tr>
            {columns.map((c, i) => (
              <th key={i} scope="col" className={numeric.includes(i) ? 'rp-num' : undefined}>
                {c}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((r, i) => (
            <tr key={i}>
              {r.map((c, j) => (
                <td key={j} className={numeric.includes(j) ? 'rp-num' : undefined}>
                  {c}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

/** A chart's card: title, subtitle, legend, and a toggle to the chart's table view. */
export function ChartCard({ title, subtitle, legend, table, actions, children, className }: { title: ReactNode; subtitle?: ReactNode; legend?: LegendItem[]; table?: TableData; actions?: ReactNode; children: ReactNode; className?: string }) {
  const [asTable, setAsTable] = useState(false)
  const id = useId()
  return (
    <section className={`rp-card rp-viz ${className ?? ''}`} aria-labelledby={id}>
      <header className="rp-card-head">
        <div className="rp-card-titles">
          <h2 id={id}>{title}</h2>
          {subtitle && <div className="rp-card-sub">{subtitle}</div>}
        </div>
        {actions}
        {table && (
          <button
            type="button"
            className="icon-btn"
            aria-pressed={asTable}
            title={asTable ? t('Show chart') : t('Show as table')}
            aria-label={asTable ? t('Show chart') : t('Show as table')}
            onClick={() => setAsTable((v) => !v)}
          >
            {asTable ? <ChartColumn size={16} /> : <Table2 size={16} />}
          </button>
        )}
      </header>
      {legend && legend.length > 0 && !asTable && <Legend items={legend} />}
      {asTable && table ? <DataTable {...table} /> : children}
    </section>
  )
}

// ---------- line / step chart ----------

export interface LinePoint {
  x: number
  y: number
  /** a marker on this point: filled dot, or a hollow ring */
  marker?: 'dot' | 'ring'
}

export interface LineSeries {
  id: string
  label: string
  color: string
  points: LinePoint[]
  step?: boolean
  dash?: boolean
  /** a 10% wash between the line and zero */
  wash?: boolean
}

function valueAt(s: LineSeries, X: number): number | null {
  const p = s.points
  if (!p.length || X < p[0].x - 1 || X > p[p.length - 1].x + 1) return null
  let i = 0
  while (i < p.length - 1 && p[i + 1].x <= X) i++
  if (s.step || i === p.length - 1) return p[i].y
  const a = p[i]
  const b = p[i + 1]
  return a.y + ((X - a.x) / (b.x - a.x || 1)) * (b.y - a.y)
}

/** Lines over time (x in ms). The crosshair snaps to `stops`; `tip(i)` describes stop i. */
export function LineChart({
  summary,
  height = 260,
  series,
  stops,
  tip,
  x0,
  x1,
  yRefs = [],
  xRefs = [],
  integer,
  onActivate,
  hint,
}: {
  summary: string
  height?: number
  series: LineSeries[]
  stops: number[]
  tip: (i: number) => ReactNode
  x0?: number
  x1?: number
  yRefs?: RefLine[]
  /** vertical markers, e.g. today */
  xRefs?: RefLine[]
  integer?: boolean
  onActivate?: (i: number) => void
  hint?: string
}) {
  const build = (w: number): Built => {
    const allX = [...stops, ...series.flatMap((s) => s.points.map((p) => p.x))]
    const lo = x0 ?? Math.min(...allX)
    const hi = x1 ?? Math.max(...allX)
    const ys = [...series.flatMap((s) => s.points.map((p) => p.y)), ...yRefs.map((r) => r.value)]
    const ticks = niceTicks(Math.min(0, ...ys), Math.max(0, ...ys), height < 200 ? 3 : 5, integer)
    const ax = yAxis(ticks)
    const m = { t: xRefs.length ? 24 : 14, r: 16, b: 28, l: ax.width }
    const x = linear(lo, hi, m.l, w - m.r)
    const y = linear(ticks[0], ticks[ticks.length - 1], height - m.b, m.t)
    const xt = timeTicks(lo, hi, Math.floor((w - m.l - m.r) / 76)).map((v) => ({ x: x(v), label: dayLabel(v) }))
    const sx = stops.map(x)
    // the marker drawn at each stop, if any: clicks only count on (or next to) it
    const markerY = stops.map((X) => {
      for (const s of series) {
        const p = s.points.find((q) => q.marker && q.x === X)
        if (p) return y(p.y)
      }
      return null
    })
    return {
      count: stops.length,
      locate: (px) => nearest(sx, px),
      near: (i, px, py) => markerY[i] !== null && Math.hypot(sx[i] - px, markerY[i]! - py) <= 16,
      tip: (i) => ({ x: sx[i], y: m.t, content: tip(i) }),
      draw: (active) => (
        <>
          <Grid ticks={ticks} labels={ax.labels} y={y} x0={m.l} x1={w - m.r} />
          <XTicks items={xt} y={height - 8} width={w} />
          {xRefs.map((r, i) => {
            const xx = crisp(x(r.value))
            const half = (r.label.length * CHAR_W) / 2
            const anchor = xx - half < m.l ? 'start' : xx + half > w ? 'end' : 'middle'
            return (
              <g key={i} aria-hidden>
                <line x1={xx} x2={xx} y1={m.t - 4} y2={height - m.b} className="rp-marker-line" />
                <text x={anchor === 'start' ? xx - 4 : anchor === 'end' ? xx + 4 : xx} y={m.t - 10} textAnchor={anchor} className="rp-reflabel">
                  {r.label}
                </text>
              </g>
            )
          })}
          {series.map((s) => {
            const pts = s.points.map((p) => [x(p.x), y(p.y)] as [number, number])
            const line = pathOf(pts, !!s.step)
            const zero = y(Math.max(ticks[0], Math.min(0, ticks[ticks.length - 1])))
            return (
              <g key={s.id}>
                {s.wash && pts.length > 1 && <path d={`${line}V${r1(zero)}H${r1(pts[0][0])}Z`} className="rp-wash" style={{ fill: s.color }} />}
                <path d={line} className={`rp-line ${s.dash ? 'rp-line-dash' : ''}`} style={{ stroke: s.color }} />
              </g>
            )
          })}
          <RefLines refs={yRefs} y={y} x0={m.l} x1={w - m.r} />
          {series.map((s) =>
            s.points.map((p, i) =>
              p.marker ? <circle key={`${s.id}-${i}`} cx={r1(x(p.x))} cy={r1(y(p.y))} r={4} className={`rp-dot rp-dot-${p.marker}`} style={{ '--c': s.color } as CSSProperties} /> : null,
            ),
          )}
          {active !== null && (
            <g aria-hidden>
              <line x1={crisp(sx[active])} x2={crisp(sx[active])} y1={m.t} y2={height - m.b} className="rp-crosshair" />
              {series.map((s) => {
                const v = valueAt(s, stops[active])
                return v === null ? null : <circle key={s.id} cx={r1(sx[active])} cy={r1(y(v))} r={5.5} className="rp-dot rp-dot-active" style={{ '--c': s.color } as CSSProperties} />
              })}
            </g>
          )}
        </>
      ),
    }
  }
  return <Plot summary={summary} height={height} build={build} onActivate={onActivate} hint={hint} />
}

// ---------- grouped columns ----------

/** Vertical columns, one group per label and one column per series; `values[label][series]`. */
export function ColumnChart({
  summary,
  height = 260,
  labels,
  series,
  values,
  tip,
  yRefs = [],
  integer,
  labelSeries,
  hint,
}: {
  summary: string
  height?: number
  labels: string[]
  series: { id: string; label: string; color: string }[]
  values: number[][]
  tip: (i: number) => ReactNode
  yRefs?: RefLine[]
  integer?: boolean
  /** the series whose values are printed on the column caps (where they fit) */
  labelSeries?: number
  hint?: string
}) {
  const build = (w: number): Built => {
    const n = labels.length
    const ns = series.length
    const ticks = niceTicks(0, Math.max(0, ...values.flat(), ...yRefs.map((r) => r.value)), height < 200 ? 3 : 5, integer)
    const ax = yAxis(ticks)
    const m = { t: 18, r: 16, b: 28, l: ax.width }
    const y = linear(ticks[0], ticks[ticks.length - 1], height - m.b, m.t)
    const band = (w - m.l - m.r) / Math.max(1, n)
    const gap = 2
    const barW = Math.max(1, Math.min(24, (band * 0.72 - gap * (ns - 1)) / ns))
    const groupW = barW * ns + gap * (ns - 1)
    const bx = (i: number, j: number) => m.l + i * band + (band - groupW) / 2 + j * (barW + gap)
    const longest = Math.min(18, Math.max(1, ...labels.map((l) => l.length)))
    const every = Math.max(1, Math.ceil((longest * CHAR_W + 10) / band))
    const room = Math.max(3, Math.floor((band * every - 10) / CHAR_W))
    const fit = (s: string) => (s.length > room ? s.slice(0, room - 1) + '…' : s)
    return {
      count: n,
      locate: (px) => (px < m.l || px > w - m.r ? null : Math.min(n - 1, Math.max(0, Math.floor((px - m.l) / band)))),
      tip: (i) => ({ x: m.l + i * band + band / 2, y: m.t, content: tip(i), offset: groupW / 2 + 10 }),
      draw: (active) => (
        <>
          <Grid ticks={ticks} labels={ax.labels} y={y} x0={m.l} x1={w - m.r} />
          {active !== null && <rect x={m.l + active * band + 1} y={m.t - 6} width={Math.max(0, band - 2)} height={height - m.b - m.t + 6} rx={4} className="rp-hover-band" />}
          {values.map((row, i) =>
            row.map((v, j) => (v > 0 ? <path key={`${i}-${j}`} d={colPath(bx(i, j), y(v), barW, y(0) - y(v))} style={{ fill: series[j].color }} /> : null)),
          )}
          <line x1={m.l} x2={w - m.r} y1={crisp(y(0))} y2={crisp(y(0))} className="rp-baseline" />
          {labelSeries !== undefined &&
            values.map((row, i) => {
              const v = row[labelSeries] ?? 0
              const text = fmt(v)
              if (text.length * CHAR_W + 4 > band / ns + barW / 2) return null
              return (
                <text key={i} x={bx(i, labelSeries) + barW / 2} y={y(v) - 5} textAnchor="middle" className="rp-cap" aria-hidden>
                  {text}
                </text>
              )
            })}
          <XTicks items={labels.flatMap((l, i) => (i % every === 0 ? [{ x: m.l + i * band + band / 2, label: fit(l) }] : []))} y={height - 8} width={w} />
          <RefLines refs={yRefs} y={y} x0={m.l} x1={w - m.r} />
        </>
      ),
    }
  }
  return <Plot summary={summary} height={height} build={build} hint={hint} />
}

// ---------- stacked areas ----------

/** Stacked areas over days; `series` from the bottom of the stack up. */
export function StackedArea({ summary, height = 280, xs, series, tip, hint }: { summary: string; height?: number; xs: number[]; series: { id: string; label: string; color: string; values: number[] }[]; tip: (i: number) => ReactNode; hint?: string }) {
  const build = (w: number): Built => {
    const totals = xs.map((_, i) => series.reduce((a, s) => a + (s.values[i] ?? 0), 0))
    const ticks = niceTicks(0, Math.max(0, ...totals), height < 200 ? 3 : 5, true)
    const ax = yAxis(ticks)
    const m = { t: 14, r: 16, b: 28, l: ax.width }
    const x = linear(xs[0], xs[xs.length - 1], m.l, w - m.r)
    const y = linear(ticks[0], ticks[ticks.length - 1], height - m.b, m.t)
    const px = xs.map(x)
    const lower: number[][] = []
    const upper: number[][] = []
    const acc = xs.map(() => 0)
    series.forEach((s, k) => {
      lower[k] = [...acc]
      xs.forEach((_, i) => (acc[i] += s.values[i] ?? 0))
      upper[k] = [...acc]
    })
    const xt = timeTicks(xs[0], xs[xs.length - 1], Math.floor((w - m.l - m.r) / 76)).map((v) => ({ x: x(v), label: dayLabel(v) }))
    return {
      count: xs.length,
      locate: (p) => nearest(px, p),
      tip: (i) => ({ x: px[i], y: m.t, content: tip(i) }),
      draw: (active) => (
        <>
          <Grid ticks={ticks} labels={ax.labels} y={y} x0={m.l} x1={w - m.r} />
          {series.map((s, k) => {
            const top = px.map((xx, i) => [xx, y(upper[k][i])] as [number, number])
            const bottom = px.map((xx, i) => [xx, y(lower[k][i])] as [number, number]).reverse()
            return <path key={s.id} d={`${pathOf(top, false)}L${pathOf(bottom, false).slice(1)}Z`} className="rp-band" style={{ fill: s.color }} />
          })}
          {/* edges top-down, so where a band is empty the edge of the band below it shows */}
          {[...series.keys()].reverse().map((k) => (
            <path key={series[k].id} d={pathOf(px.map((xx, i) => [xx, y(upper[k][i])]), false)} className="rp-line rp-line-edge" style={{ stroke: series[k].color }} />
          ))}
          <line x1={m.l} x2={w - m.r} y1={crisp(y(0))} y2={crisp(y(0))} className="rp-baseline" />
          <XTicks items={xt} y={height - 8} width={w} />
          {active !== null && (
            <g aria-hidden>
              <line x1={crisp(px[active])} x2={crisp(px[active])} y1={m.t} y2={height - m.b} className="rp-crosshair" />
              {series.map((s, k) =>
                (s.values[active] ?? 0) > 0 ? <circle key={s.id} cx={r1(px[active])} cy={r1(y(upper[k][active]))} r={4.5} className="rp-dot rp-dot-active" style={{ '--c': s.color } as CSSProperties} /> : null,
              )}
            </g>
          )}
        </>
      ),
    }
  }
  return <Plot summary={summary} height={height} build={build} hint={hint} />
}

// ---------- scatter ----------

/** Dots over time; the pointer picks the nearest dot (within reach), arrows walk them in order. */
export function Scatter({
  summary,
  height = 300,
  points,
  x0,
  x1,
  tip,
  yRefs = [],
  onActivate,
  color = SERIES[0],
  hint,
}: {
  summary: string
  height?: number
  points: { x: number; y: number }[]
  x0: number
  x1: number
  tip: (i: number) => ReactNode
  yRefs?: RefLine[]
  onActivate?: (i: number) => void
  color?: string
  hint?: string
}) {
  const build = (w: number): Built => {
    const ticks = niceTicks(0, Math.max(1, ...points.map((p) => p.y), ...yRefs.map((r) => r.value)), height < 200 ? 3 : 5)
    const ax = yAxis(ticks)
    const m = { t: 14, r: 16, b: 28, l: ax.width }
    const x = linear(x0, x1, m.l + 6, w - m.r - 6)
    const y = linear(ticks[0], ticks[ticks.length - 1], height - m.b, m.t)
    const pos = points.map((p) => [x(p.x), y(p.y)] as [number, number])
    const xt = timeTicks(x0, x1, Math.floor((w - m.l - m.r) / 76)).map((v) => ({ x: x(v), label: dayLabel(v) }))
    return {
      count: points.length,
      locate: (px, py) => {
        let best: number | null = null
        let dist = 18
        pos.forEach(([a, b], i) => {
          const d = Math.hypot(a - px, b - py)
          if (d <= dist) {
            dist = d
            best = i
          }
        })
        return best
      },
      tip: (i) => ({ x: pos[i][0], y: Math.max(0, pos[i][1] - 16), content: tip(i), offset: 14 }),
      draw: (active) => (
        <>
          <Grid ticks={ticks} labels={ax.labels} y={y} x0={m.l} x1={w - m.r} />
          <XTicks items={xt} y={height - 8} width={w} />
          <RefLines refs={yRefs} y={y} x0={m.l} x1={w - m.r} />
          {pos.map(([a, b], i) => (
            <circle key={i} cx={r1(a)} cy={r1(b)} r={4.5} className="rp-dot rp-dot-dot" style={{ '--c': color } as CSSProperties} />
          ))}
          {active !== null && <circle cx={r1(pos[active][0])} cy={r1(pos[active][1])} r={7} className="rp-dot rp-dot-active" style={{ '--c': color } as CSSProperties} />}
        </>
      ),
    }
  }
  return <Plot summary={summary} height={height} build={build} onActivate={onActivate} hint={hint} />
}

// ---------- horizontal bar list (an HTML table, so it is its own table view) ----------

export interface BarSegment {
  value: number
  color: string
  label: string
}

export interface BarRow {
  id: string
  label: ReactNode
  /** plain-text name for the accessible description */
  name: string
  segments: BarSegment[]
  /** extra cells after the total */
  cells?: ReactNode[]
}

export function BarList({
  rows,
  summary,
  nameHeader,
  valueHeader,
  columns = [],
  max,
  shareOf,
}: {
  rows: BarRow[]
  summary: string
  nameHeader: string
  valueHeader: string
  columns?: string[]
  max?: number
  /** a whole to show each row's share of in the tooltip */
  shareOf?: number
}) {
  const [hover, setHover] = useState<string | null>(null)
  const sum = (r: BarRow) => r.segments.reduce((a, s) => a + s.value, 0)
  const top = max ?? Math.max(1, ...rows.map(sum))
  return (
    <table className="rp-bartable rp-viz">
      <caption className="sr-only">{summary}</caption>
      <thead className={columns.length ? undefined : 'sr-only'}>
        <tr>
          <th scope="col">{nameHeader}</th>
          <th scope="col">
            <span className="sr-only">{t('Chart')}</span>
          </th>
          <th scope="col" className="rp-num">
            {valueHeader}
          </th>
          {columns.map((c) => (
            <th key={c} scope="col" className="rp-num">
              {c}
            </th>
          ))}
        </tr>
      </thead>
      <tbody>
        {rows.map((r) => {
          const total = sum(r)
          const segs = r.segments.filter((s) => s.value > 0)
          const describe = r.segments.length > 1 ? r.segments.map((s) => `${s.label}: ${fmt(s.value)}`).join(', ') : fmt(total)
          return (
            <tr key={r.id} className={hover === r.id ? 'rp-hover' : undefined} onPointerEnter={() => setHover(r.id)} onPointerLeave={() => setHover((h) => (h === r.id ? null : h))}>
              <th scope="row" className="rp-bar-name">
                {r.label}
              </th>
              <td className="rp-bar-cell">
                <div className="rp-bar" role="img" aria-label={`${r.name}: ${describe}`}>
                  {segs.map((s, i) => (
                    <span key={i} className="rp-seg" style={{ width: `calc(${(s.value / top) * 100}% - ${i ? 2 : 0}px)`, background: s.color }} />
                  ))}
                </div>
                {hover === r.id && (
                  <div className="rp-tip rp-bar-tip" style={{ insetInlineStart: `min(${(total / top) * 100}%, calc(100% - 180px))` }} aria-hidden>
                    <TipBody
                      title={r.name}
                      rows={r.segments.map((s) => ({ color: s.color, value: fmt(s.value), label: s.label }))}
                      note={shareOf ? t('{percent} of {total}', { percent: pct(total, shareOf), total: fmt(shareOf) }) : undefined}
                    />
                  </div>
                )}
              </td>
              <td className="rp-num rp-bar-total">{fmt(total)}</td>
              {r.cells?.map((c, i) => (
                <td key={i} className="rp-num">
                  {c}
                </td>
              ))}
            </tr>
          )
        })}
      </tbody>
    </table>
  )
}
