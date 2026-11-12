import { t } from '../i18n'
import './Reports.css'
import { useMemo, useState, type ReactNode } from 'react'
import { Link, useParams, useSearchParams } from 'react-router-dom'
import { keepPreviousData, useQuery, type UseQueryResult } from '@tanstack/react-query'
import { ChartArea, ChartColumn, ChartScatter, ChevronDown, CircleCheck, Hourglass, Inbox, TrendingDown, TriangleExclamationPoint } from 'lucide-react'
import { api, dateTime, qs, shortDate, PRIORITIES, type Category, type Priority, type Project } from '../api'
import { Avatar, Empty, ErrorBox, Loading, PopButton, Tabs, Unassigned, useProject } from '../ui'
import { PriorityIcon, TypeIcon, priorityLabel } from '../icons'
import { useOpenIssue } from '../components/Layout'
import {
  BarList, CATEGORY_COLOR, ChartCard, ColumnChart, DAY, LineChart, MUTED, REF, SERIES, Scatter, StackedArea, TipBody, dayLabel, dayMs, fmt, pct,
  type BarRow, type LegendItem,
} from '../components/charts'

// ---------- report payloads (mirror crates/server/src/reports.rs) ----------

type Unit = 'points' | 'issues'

interface Overview {
  categories: Partial<Record<Category, number>>
  statuses: { id: string; name: string; category: Category; color: string | null; count: number }[]
  types: { name: string; icon: string; color: string; count: number }[]
  priorities: { priority: Priority; count: number }[]
  overdue: number
  unassigned: number
  createdLastWeek: number
  resolvedLastWeek: number
  stale: number
}

interface SprintInfo {
  id: string
  name: string
  state: 'future' | 'active' | 'closed'
  goal: string | null
  startAt: string | null
  endAt: string | null
  completedAt: string | null
}

type StepEvent = 'start' | 'completed' | 'reopened' | 'added' | 'removed' | 'estimate' | 'changed'

interface Burndown {
  sprint: SprintInfo
  unit: Unit
  committed: number
  committedIssues: number
  remaining: number
  completed: number
  completedIssues: number
  added: number
  removed: number
  start: string
  end: string
  now: string
  steps: { at: string; remaining: number; event: StepEvent; key?: string | null }[]
}

interface Velocity {
  sprints: { id: string; name: string; startAt: string | null; completedAt: string | null; committed: number; completed: number; committedIssues: number; completedIssues: number; unit: Unit }[]
  average: number
  unit: Unit
}

interface Flow {
  statuses: { id: string; name: string; category: Category; color: string | null }[]
  days: { date: string; counts: Record<string, number> }[]
}

interface TimeStats {
  average: number
  median: number
  p85: number
}

interface CycleTime {
  issues: { key: string; title: string; type: string; resolvedAt: string; cycleDays: number; leadDays: number }[]
  cycle: TimeStats
  lead: TimeStats
}

interface CreatedResolved {
  days: { date: string; created: number; resolved: number }[]
}

interface Workload {
  people: { id: string | null; name: string | null; todo: number; inProgress: number; points: number; overdue: number }[]
}

type ReportId = 'overview' | 'burndown' | 'velocity' | 'cumulative-flow' | 'cycle-time' | 'created-resolved' | 'workload'

// ---------- helpers ----------

function useReport<T>(project: string, report: string, params: Record<string, number> = {}) {
  return useQuery({
    queryKey: ['report', project, report, params],
    queryFn: () => api.get<T>(`/projects/${encodeURIComponent(project)}/reports/${report}${qs(params)}`),
    placeholderData: keepPreviousData,
  })
}

/** The report's time range, kept in the URL (`?days=`). */
function useRange(options: number[], fallback: number): [number, (d: number) => void] {
  const [params, setParams] = useSearchParams()
  const v = Number(params.get('days'))
  const days = options.includes(v) ? v : fallback
  const set = (d: number) =>
    setParams(
      (prev) => {
        const n = new URLSearchParams(prev)
        if (d === fallback) n.delete('days')
        else n.set('days', String(d))
        return n
      },
      { replace: true },
    )
  return [days, set]
}

const categoryLabel = (c: Category) => (c === 'todo' ? t('To do') : c === 'in_progress' ? t('In progress') : t('Done'))
const sum = (xs: number[]) => xs.reduce((a, b) => a + b, 0)
const signed = (v: number) => (v > 0 ? `+${fmt(v)}` : v < 0 ? `−${fmt(-v)}` : fmt(0))
const daysText = (v: number) => t('{count} days', { count: fmt(v) })
const amount = (unit: Unit, v: number) => (unit === 'points' ? t('{count} points', { count: fmt(v) }) : t('{count} issues', { count: fmt(v) }))

function Seg<T extends string | number>({ value, options, onChange, label }: { value: T; options: { id: T; label: string }[]; onChange: (v: T) => void; label: string }) {
  return (
    <div className="seg rp-seg-sm" role="group" aria-label={label}>
      {options.map((o) => (
        <button key={o.id} type="button" className={o.id === value ? 'active' : undefined} aria-pressed={o.id === value} onClick={() => onChange(o.id)}>
          {o.label}
        </button>
      ))}
    </div>
  )
}

function RangeSwitch({ days, options, onChange }: { days: number; options: number[]; onChange: (d: number) => void }) {
  return <Seg value={days} onChange={onChange} label={t('Time range')} options={options.map((d) => ({ id: d, label: t('{count} days', { count: d }) }))} />
}

function Tile({ value, label, sub, tone, icon }: { value: ReactNode; label: string; sub?: ReactNode; tone?: 'danger' | 'warning'; icon?: ReactNode }) {
  return (
    <div className={`stat rp-stat ${tone ? `rp-tone-${tone}` : ''}`}>
      <div className="stat-value">
        {tone && icon}
        {value}
      </div>
      <div className="stat-label">{label}</div>
      {sub && <div className="rp-stat-sub">{sub}</div>}
    </div>
  )
}

/** Loading / error / content, holding the previous render (dimmed) while a new range loads. */
function Loaded<T>({ q, children }: { q: UseQueryResult<T>; children: (data: T) => ReactNode }) {
  if (q.isLoading) return <Loading />
  if (q.error) return <ErrorBox error={q.error} />
  if (!q.data) return null
  return (
    <div className={`rp-body ${q.isPlaceholderData ? 'rp-stale' : ''}`} aria-busy={q.isPlaceholderData || undefined}>
      {children(q.data)}
    </div>
  )
}

// ---------- page ----------

export default function Reports() {
  const { key } = useParams()
  const project = useProject(key)
  const [params, setParams] = useSearchParams()
  if (project.isLoading) return <Loading />
  if (project.error) return <div className="page"><ErrorBox error={project.error} /></div>
  if (!project.data) return null
  const p = project.data
  const tabs: { id: ReportId; label: string }[] = [
    { id: 'overview', label: t('Overview') },
    ...(p.kind === 'scrum'
      ? [
          { id: 'burndown' as const, label: t('Burndown') },
          { id: 'velocity' as const, label: t('Velocity') },
        ]
      : []),
    { id: 'cumulative-flow', label: t('Cumulative flow') },
    { id: 'cycle-time', label: t('Cycle time') },
    { id: 'created-resolved', label: t('Created vs resolved') },
    { id: 'workload', label: t('Workload') },
  ]
  const tab = tabs.find((x) => x.id === params.get('report'))?.id ?? 'overview'
  const choose = (id: ReportId) =>
    setParams(
      (prev) => {
        const n = new URLSearchParams(prev)
        n.set('report', id)
        n.delete('days')
        return n
      },
      { replace: true },
    )
  return (
    <div className="page rp-page rp-viz">
      <div className="crumbs">
        <Link to="/projects">{t('Projects')}</Link>
        <span>/</span>
        <Link to={`/projects/${p.key}`}>{p.name}</Link>
      </div>
      <div className="page-head">
        <h1>{t('Reports')}</h1>
      </div>
      <Tabs value={tab} onChange={choose} tabs={tabs} />
      <div role="tabpanel" aria-label={tabs.find((x) => x.id === tab)?.label} className="rp-panel">
        {tab === 'overview' && <OverviewReport project={p} />}
        {tab === 'burndown' && <BurndownReport project={p} />}
        {tab === 'velocity' && <VelocityReport project={p} />}
        {tab === 'cumulative-flow' && <FlowReport project={p} />}
        {tab === 'cycle-time' && <CycleReport project={p} />}
        {tab === 'created-resolved' && <CreatedReport project={p} />}
        {tab === 'workload' && <WorkloadReport project={p} />}
      </div>
    </div>
  )
}

// ---------- overview ----------

function OverviewReport({ project }: { project: Project }) {
  const q = useReport<Overview>(project.key, 'overview')
  return (
    <Loaded q={q}>
      {(d) => {
        const total = sum(d.statuses.map((s) => s.count))
        const open = (d.categories.todo ?? 0) + (d.categories.in_progress ?? 0)
        const cats = (['todo', 'in_progress', 'done'] as Category[]).filter((c) => d.statuses.some((s) => s.category === c))
        const prio = new Map(d.priorities.map((x) => [x.priority, x.count]))
        return (
          <>
            <div className="stat-row">
              <Tile value={fmt(d.overdue)} label={t('Overdue')} tone={d.overdue ? 'danger' : undefined} icon={<TriangleExclamationPoint size={18} aria-hidden />} />
              <Tile value={fmt(d.unassigned)} label={t('Open and unassigned')} />
              <Tile value={fmt(d.createdLastWeek)} label={t('Created in the last 7 days')} />
              <Tile value={fmt(d.resolvedLastWeek)} label={t('Resolved in the last 7 days')} />
              <Tile value={fmt(d.stale)} label={t('In progress for more than 7 days')} tone={d.stale ? 'warning' : undefined} icon={<Hourglass size={18} aria-hidden />} />
            </div>
            {total === 0 ? (
              <Empty icon={<Inbox size={32} />} title={t('No issues yet')}>
                {t('Charts appear here once the project has issues.')}
              </Empty>
            ) : (
              <div className="rp-grid">
                <ChartCard
                  title={t('Issues by status')}
                  subtitle={t('{count} issues · {percent} done', { count: fmt(total), percent: pct(d.categories.done ?? 0, total) })}
                  legend={cats.map((c) => ({ label: categoryLabel(c), color: CATEGORY_COLOR[c] }))}
                >
                  <BarList
                    summary={t('Issues by status')}
                    nameHeader={t('Status')}
                    valueHeader={t('Issues')}
                    shareOf={total}
                    rows={d.statuses.map((s) => ({ id: s.id, name: s.name, label: s.name, segments: [{ value: s.count, color: CATEGORY_COLOR[s.category], label: categoryLabel(s.category) }] }))}
                  />
                </ChartCard>
                <ChartCard title={t('Open issues by type')} subtitle={t('{count} open', { count: fmt(open) })}>
                  <BarList
                    summary={t('Open issues by type')}
                    nameHeader={t('Type')}
                    valueHeader={t('Open')}
                    shareOf={open}
                    rows={d.types.map((x) => ({
                      id: x.name,
                      name: x.name,
                      label: (
                        <span className="rp-name">
                          <TypeIcon icon={x.icon} color={x.color} />
                          <span>{x.name}</span>
                        </span>
                      ),
                      segments: [{ value: x.count, color: SERIES[0], label: t('Open') }],
                    }))}
                  />
                </ChartCard>
                <ChartCard title={t('Open issues by priority')} subtitle={t('{count} open', { count: fmt(open) })}>
                  <BarList
                    summary={t('Open issues by priority')}
                    nameHeader={t('Priority')}
                    valueHeader={t('Open')}
                    shareOf={open}
                    rows={PRIORITIES.map((pr) => ({
                      id: pr,
                      name: priorityLabel(pr),
                      label: <PriorityIcon priority={pr} withLabel />,
                      segments: [{ value: prio.get(pr) ?? 0, color: SERIES[0], label: t('Open') }],
                    }))}
                  />
                </ChartCard>
              </div>
            )}
          </>
        )
      }}
    </Loaded>
  )
}

// ---------- burndown ----------

function BurndownReport({ project }: { project: Project }) {
  const [params, setParams] = useSearchParams()
  const open = useOpenIssue()
  const sprints = useQuery({
    queryKey: ['report', project.key, 'sprints'],
    queryFn: () => api.get<SprintInfo[]>(`/projects/${encodeURIComponent(project.key)}/sprints`),
  })
  const list = useMemo(() => {
    const time = (s: SprintInfo) => Date.parse(s.completedAt ?? s.startAt ?? '') || 0
    return (sprints.data ?? []).filter((s) => s.state === 'active' || s.state === 'closed').sort((a, b) => (a.state === b.state ? time(b) - time(a) : a.state === 'active' ? -1 : 1))
  }, [sprints.data])
  const sprint = list.find((s) => s.id === params.get('sprint')) ?? list[0]
  const bd = useQuery({
    queryKey: ['report', 'burndown', sprint?.id],
    queryFn: () => api.get<Burndown>(`/sprints/${sprint!.id}/burndown`),
    enabled: !!sprint,
    placeholderData: keepPreviousData,
  })
  if (sprints.isLoading) return <Loading />
  if (sprints.error) return <ErrorBox error={sprints.error} />
  if (!sprint)
    return (
      <Empty icon={<TrendingDown size={32} />} title={t('No sprints to chart yet')}>
        {t('Start a sprint from the backlog to follow its burndown here.')}
      </Empty>
    )
  const pick = (id: string) =>
    setParams(
      (prev) => {
        const n = new URLSearchParams(prev)
        n.set('sprint', id)
        return n
      },
      { replace: true },
    )
  const picker = (
    <PopButton className="btn btn-sm" label={<>{sprint.name} <ChevronDown size={14} aria-hidden /></>} title={t('Choose a sprint')} width={260}>
      {(close) =>
        list.map((s) => (
          <button
            key={s.id}
            type="button"
            className={`menu-item ${s.id === sprint.id ? 'selected' : ''}`}
            onClick={() => {
              close()
              pick(s.id)
            }}
          >
            <span style={{ flex: 1 }}>{s.name}</span>
            <span className="mi-desc">{s.state === 'active' ? t('Active') : t('Closed {date}', { date: shortDate(s.completedAt ?? s.endAt) })}</span>
          </button>
        ))
      }
    </PopButton>
  )
  return (
    <>
      <div className="toolbar">
        {picker}
        {sprint.goal && <span className="muted small rp-goal">{sprint.goal}</span>}
      </div>
      <Loaded q={bd}>{(b) => <BurndownView b={b} onOpen={open} />}</Loaded>
    </>
  )
}

function stepText(s: Burndown['steps'][number]): string {
  const key = s.key ?? ''
  switch (s.event) {
    case 'start':
      return t('Sprint started')
    case 'completed':
      return t('{key} completed', { key })
    case 'reopened':
      return t('{key} reopened', { key })
    case 'added':
      return t('{key} added to the sprint', { key })
    case 'removed':
      return t('{key} removed from the sprint', { key })
    case 'estimate':
      return t('{key} re-estimated', { key })
    default:
      return t('{key} changed', { key })
  }
}

function BurndownView({ b, onOpen }: { b: Burndown; onOpen: (key: string) => void }) {
  const unit = b.unit
  const points = unit === 'points'
  const closed = b.sprint.state === 'closed'
  const start = Date.parse(b.start)
  const end = Date.parse(b.end)
  const now = Date.parse(b.now)
  const steps = b.steps
  const lastStep = steps[steps.length - 1]
  const lastAt = lastStep ? Date.parse(lastStep.at) : start
  const lineEnd = Math.max(lastAt, closed ? Date.parse(b.sprint.completedAt ?? b.end) : now)
  const x1 = Math.max(end, lineEnd)
  const ideal = (x: number) => Math.max(0, b.committed * (1 - (x - start) / Math.max(1, end - start)))
  const remainingPoints = steps.map((s, i) => ({ x: Date.parse(s.at), y: s.remaining, marker: i === 0 ? undefined : s.event === 'completed' ? ('dot' as const) : ('ring' as const) }))
  const tail = lineEnd > lastAt && lastStep ? [{ x: lineEnd, y: lastStep.remaining }] : []
  const stops = [...remainingPoints.map((p) => p.x), ...tail.map((p) => p.x)]
  const daysLeft = Math.ceil((end - now) / DAY)
  const legend: LegendItem[] = [
    { label: t('Remaining'), color: SERIES[0], kind: 'line' },
    { label: t('Ideal'), color: REF, kind: 'dash' },
    { label: t('Issue completed'), color: SERIES[0], kind: 'dot' },
    { label: t('Scope or estimate changed'), color: SERIES[0], kind: 'ring' },
  ]
  const tip = (i: number) => {
    const step = steps[i]
    const x = stops[i]
    const remaining = step ? step.remaining : (lastStep?.remaining ?? 0)
    return (
      <TipBody
        title={step ? stepText(step) : closed ? t('Sprint completed') : t('Now')}
        rows={[
          { color: SERIES[0], value: amount(unit, remaining), label: t('Remaining') },
          { color: REF, kind: 'dash', value: amount(unit, Math.round(ideal(x) * 10) / 10), label: t('Ideal') },
        ]}
        note={
          <>
            {dateTime(new Date(x).toISOString())}
            {step?.key && <div className="rp-tip-hint">{t('Press Enter or click to open {key}', { key: step.key })}</div>}
          </>
        }
      />
    )
  }
  return (
    <>
      <div className="stat-row">
        <Tile value={fmt(b.committed)} label={points ? t('Points committed at the start') : t('Issues committed at the start')} sub={points ? t('{count} issues', { count: b.committedIssues }) : undefined} />
        <Tile value={fmt(b.completed)} label={points ? t('Points completed') : t('Issues completed')} sub={points ? t('{count} issues', { count: b.completedIssues }) : undefined} />
        <Tile value={fmt(b.remaining)} label={points ? t('Points remaining') : t('Issues remaining')} />
        <Tile value={`+${fmt(b.added)} / −${fmt(b.removed)}`} label={t('Issues added / removed')} />
        {closed ? (
          <Tile value={shortDate(b.sprint.completedAt)} label={t('Sprint completed')} />
        ) : daysLeft >= 0 ? (
          <Tile value={fmt(daysLeft)} label={t('Days left')} sub={t('Ends {date}', { date: shortDate(b.end) })} />
        ) : (
          <Tile value={fmt(-daysLeft)} label={t('Days past the end date')} tone="warning" icon={<Hourglass size={18} aria-hidden />} />
        )}
      </div>
      <ChartCard
        title={t('Burndown')}
        subtitle={unit === 'points' ? t('Remaining story points, from the sprint start to its end') : t('Remaining issues, from the sprint start to its end (no estimates in this sprint)')}
        legend={legend}
        table={{
          columns: [t('Time'), t('Change'), t('Remaining')],
          rows: steps.map((s) => [dateTime(s.at), stepText(s), fmt(s.remaining)]),
          numeric: [2],
        }}
      >
        <LineChart
          summary={t('Burndown of {sprint}: {remaining} remaining of {committed} committed.', { sprint: b.sprint.name, remaining: amount(unit, b.remaining), committed: amount(unit, b.committed) })}
          height={300}
          x0={start}
          x1={x1}
          integer={unit === 'issues'}
          series={[
            { id: 'ideal', label: t('Ideal'), color: REF, dash: true, points: [{ x: start, y: b.committed }, { x: end, y: 0 }] },
            { id: 'remaining', label: t('Remaining'), color: SERIES[0], step: true, points: [...remainingPoints, ...tail] },
          ]}
          stops={stops}
          tip={tip}
          xRefs={closed ? [] : [{ value: now, label: t('Today') }]}
          onActivate={(i) => steps[i]?.key && onOpen(steps[i].key!)}
          hint={t('Use the arrow keys to step through the changes; Enter opens the issue.')}
        />
      </ChartCard>
    </>
  )
}

// ---------- velocity ----------

function VelocityReport({ project }: { project: Project }) {
  const q = useReport<Velocity>(project.key, 'velocity', { count: 8 })
  const [mode, setMode] = useState<Unit>('points')
  return (
    <Loaded q={q}>
      {(d) => {
        const s = d.sprints
        if (!s.length)
          return (
            <Empty icon={<ChartColumn size={32} />} title={t('No closed sprints yet')}>
              {t('Velocity appears once the first sprint is completed.')}
            </Empty>
          )
        const pointsOk = d.unit === 'points' && s.every((x) => x.unit === 'points')
        const unit: Unit = pointsOk && mode === 'points' ? 'points' : 'issues'
        const committed = s.map((x) => (unit === 'points' ? x.committed : x.committedIssues))
        const completed = s.map((x) => (unit === 'points' ? x.completed : x.completedIssues))
        const avg = sum(completed) / completed.length
        const last = s[s.length - 1]
        return (
          <>
            <div className="stat-row">
              <Tile value={fmt(avg)} label={unit === 'points' ? t('Average points completed per sprint') : t('Average issues completed per sprint')} sub={t('Closed sprints: {count}', { count: s.length })} />
              <Tile value={fmt(completed[completed.length - 1])} label={unit === 'points' ? t('Points completed in {sprint}', { sprint: last.name }) : t('Issues completed in {sprint}', { sprint: last.name })} />
              <Tile value={pct(sum(completed), sum(committed))} label={t('Completed of the committed work')} />
            </div>
            <ChartCard
              title={t('Velocity')}
              subtitle={unit === 'points' ? t('Story points committed at the sprint start and completed by its end') : t('Issues committed at the sprint start and completed by its end')}
              legend={[
                { label: t('Committed'), color: MUTED },
                { label: t('Completed'), color: SERIES[0] },
                { label: t('Average completed'), color: REF, kind: 'dash' },
              ]}
              actions={
                pointsOk ? (
                  <Seg value={mode} onChange={setMode} label={t('Unit')} options={[{ id: 'points', label: t('Points') }, { id: 'issues', label: t('Issues') }]} />
                ) : undefined
              }
              table={{
                columns: [t('Sprint'), t('Completed on'), t('Committed'), t('Completed')],
                rows: s.map((x, i) => [x.name, shortDate(x.completedAt), fmt(committed[i]), fmt(completed[i])]),
                numeric: [2, 3],
              }}
            >
              <ColumnChart
                summary={t('Velocity of the last {count} sprints: {average} completed on average.', { count: s.length, average: amount(unit, Math.round(avg * 10) / 10) })}
                labels={s.map((x) => x.name)}
                series={[
                  { id: 'committed', label: t('Committed'), color: MUTED },
                  { id: 'completed', label: t('Completed'), color: SERIES[0] },
                ]}
                values={s.map((_, i) => [committed[i], completed[i]])}
                yRefs={[{ value: avg, label: t('Average {value}', { value: fmt(avg) }) }]}
                labelSeries={1}
                integer={unit === 'issues'}
                tip={(i) => (
                  <TipBody
                    title={s[i].name}
                    rows={[
                      { color: MUTED, value: amount(unit, committed[i]), label: t('Committed') },
                      { color: SERIES[0], value: amount(unit, completed[i]), label: t('Completed') },
                    ]}
                    note={s[i].completedAt ? t('Closed {date}', { date: shortDate(s[i].completedAt) }) : undefined}
                  />
                )}
              />
            </ChartCard>
          </>
        )
      }}
    </Loaded>
  )
}

// ---------- cumulative flow ----------

function FlowReport({ project }: { project: Project }) {
  const [days, setDays] = useRange([14, 30, 90], 30)
  const q = useReport<Flow>(project.key, 'cumulative-flow', { days })
  return (
    <>
      <div className="toolbar">
        <RangeSwitch days={days} options={[14, 30, 90]} onChange={setDays} />
      </div>
      <Loaded q={q}>
        {(d) => {
          // one categorical slot per status in workflow order; past eight, the rest fold into "Other"
          const st = d.statuses
          const groups =
            st.length <= SERIES.length
              ? st.map((s, i) => ({ id: s.id, label: s.name, color: SERIES[i], ids: [s.id], category: s.category }))
              : [
                  ...st.slice(0, SERIES.length - 1).map((s, i) => ({ id: s.id, label: s.name, color: SERIES[i], ids: [s.id], category: s.category })),
                  { id: 'other', label: t('Other'), color: MUTED, ids: st.slice(SERIES.length - 1).map((s) => s.id), category: 'done' as Category },
                ]
          const values = groups.map((g) => d.days.map((day) => sum(g.ids.map((id) => day.counts[id] ?? 0))))
          const totals = d.days.map((_, i) => sum(values.map((v) => v[i])))
          if (!d.days.length || totals.every((x) => x === 0))
            return (
              <Empty icon={<ChartArea size={32} />} title={t('No issues in this period')}>
                {t('The chart fills in as issues are created and move through the workflow.')}
              </Empty>
            )
          const byCat = (c: Category, i: number) => sum(groups.map((g, k) => (g.category === c ? values[k][i] : 0)))
          const lastI = d.days.length - 1
          const wip = d.days.map((_, i) => byCat('in_progress', i))
          const doneDelta = byCat('done', lastI) - byCat('done', 0)
          const xs = d.days.map((x) => dayMs(x.date))
          // done at the bottom: the stack runs in the workflow's reverse order
          const stack = groups.map((g, k) => ({ id: g.id, label: g.label, color: g.color, values: values[k] })).reverse()
          return (
            <>
              <div className="stat-row">
                <Tile value={fmt(wip[lastI])} label={t('In progress today')} />
                <Tile value={fmt(sum(wip) / wip.length)} label={t('Average in progress')} />
                <Tile value={fmt(byCat('todo', lastI))} label={t('To do today')} />
                <Tile value={signed(doneDelta)} label={t('Change in done over {count} days', { count: days })} />
              </div>
              <ChartCard
                title={t('Cumulative flow')}
                subtitle={t('Issues in each status at the end of each day')}
                legend={groups.map((g) => ({ label: g.label, color: g.color }))}
                table={{
                  columns: [t('Date'), ...groups.map((g) => g.label), t('Total')],
                  rows: d.days.map((day, i) => [shortDate(day.date), ...values.map((v) => fmt(v[i])), fmt(totals[i])]),
                  numeric: groups.map((_, k) => k + 1).concat(groups.length + 1),
                }}
              >
                <StackedArea
                  summary={t('Cumulative flow over {count} days: {total} issues today, {wip} in progress.', { count: days, total: fmt(totals[lastI]), wip: fmt(wip[lastI]) })}
                  height={300}
                  xs={xs}
                  series={stack}
                  tip={(i) => (
                    <TipBody
                      title={shortDate(d.days[i].date)}
                      rows={groups.map((g, k) => ({ color: g.color, value: fmt(values[k][i]), label: g.label }))}
                      note={t('Total: {count}', { count: fmt(totals[i]) })}
                    />
                  )}
                />
              </ChartCard>
            </>
          )
        }}
      </Loaded>
    </>
  )
}

// ---------- cycle time ----------

function CycleReport({ project }: { project: Project }) {
  const [days, setDays] = useRange([30, 90, 180], 90)
  const [measure, setMeasure] = useState<'cycle' | 'lead'>('cycle')
  const open = useOpenIssue()
  const q = useReport<CycleTime>(project.key, 'cycle-time', { days })
  return (
    <>
      <div className="toolbar">
        <RangeSwitch days={days} options={[30, 90, 180]} onChange={setDays} />
        <Seg
          value={measure}
          onChange={setMeasure}
          label={t('Measure')}
          options={[
            { id: 'cycle', label: t('Cycle time') },
            { id: 'lead', label: t('Lead time') },
          ]}
        />
      </div>
      <Loaded q={q}>
        {(d) => {
          if (!d.issues.length)
            return (
              <Empty icon={<ChartScatter size={32} />} title={t('No finished issues in this period')}>
                {t('Cycle times appear as issues are resolved.')}
              </Empty>
            )
          const issues = [...d.issues].sort((a, b) => Date.parse(a.resolvedAt) - Date.parse(b.resolvedAt))
          const val = (i: CycleTime['issues'][number]) => (measure === 'cycle' ? i.cycleDays : i.leadDays)
          const stats = measure === 'cycle' ? d.cycle : d.lead
          const name = measure === 'cycle' ? t('Cycle time') : t('Lead time')
          const other = measure === 'cycle' ? t('Lead time') : t('Cycle time')
          const now = Date.now()
          const slowest = [...issues].sort((a, b) => val(b) - val(a)).slice(0, 6)
          return (
            <>
              <div className="stat-row">
                <Tile value={fmt(stats.median)} label={measure === 'cycle' ? t('Median cycle time in days') : t('Median lead time in days')} />
                <Tile value={fmt(stats.p85)} label={t('85% of issues finish within this many days')} />
                <Tile value={fmt(stats.average)} label={measure === 'cycle' ? t('Average cycle time in days') : t('Average lead time in days')} />
                <Tile value={fmt(issues.length)} label={t('Issues finished in {count} days', { count: days })} />
              </div>
              <ChartCard
                title={name}
                subtitle={measure === 'cycle' ? t('Days from the first move to in progress until done, per finished issue') : t('Days from creation until done, per finished issue')}
                legend={[
                  { label: t('Finished issue'), color: SERIES[0], kind: 'dot' },
                  { label: t('Median and 85th percentile'), color: REF, kind: 'dash' },
                ]}
                table={{
                  columns: [t('Issue'), t('Title'), t('Resolved'), name],
                  rows: issues.map((i) => [i.key, i.title, shortDate(i.resolvedAt), fmt(val(i))]),
                  numeric: [3],
                }}
              >
                <Scatter
                  summary={t('{measure} of {count} issues finished in the last {days} days: median {median}, 85th percentile {p85}.', {
                    measure: name,
                    count: issues.length,
                    days,
                    median: daysText(stats.median),
                    p85: daysText(stats.p85),
                  })}
                  points={issues.map((i) => ({ x: Date.parse(i.resolvedAt), y: val(i) }))}
                  x0={now - days * DAY}
                  x1={now}
                  yRefs={[
                    { value: stats.median, label: t('Median {value}', { value: daysText(stats.median) }) },
                    { value: stats.p85, label: t('85th percentile {value}', { value: daysText(stats.p85) }) },
                  ]}
                  onActivate={(i) => open(issues[i].key)}
                  hint={t('Use the arrow keys to move between issues; Enter opens the issue.')}
                  tip={(i) => {
                    const it = issues[i]
                    return (
                      <TipBody
                        title={
                          <>
                            <span className="issue-key">{it.key}</span> {it.title}
                          </>
                        }
                        rows={[
                          { color: SERIES[0], kind: 'dot', value: daysText(val(it)), label: name },
                          { value: daysText(measure === 'cycle' ? it.leadDays : it.cycleDays), label: other },
                        ]}
                        note={t('{type} · resolved {date}', { type: it.type, date: shortDate(it.resolvedAt) })}
                      />
                    )
                  }}
                />
              </ChartCard>
              <section className="rp-card rp-slowest">
                <header className="rp-card-head">
                  <div className="rp-card-titles">
                    <h2>{t('Slowest issues')}</h2>
                    <div className="rp-card-sub">{measure === 'cycle' ? t('By cycle time') : t('By lead time')}</div>
                  </div>
                </header>
                <div className="rp-table-wrap">
                  <table className="list">
                    <thead>
                      <tr>
                        <th scope="col">{t('Issue')}</th>
                        <th scope="col">{t('Title')}</th>
                        <th scope="col">{t('Type')}</th>
                        <th scope="col" className="rp-num">{t('Cycle time')}</th>
                        <th scope="col" className="rp-num">{t('Lead time')}</th>
                        <th scope="col">{t('Resolved')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {slowest.map((i) => (
                        <tr key={i.key}>
                          <td>
                            <a href={`?issue=${i.key}`} className="issue-key" onClick={(e) => { e.preventDefault(); open(i.key) }}>
                              {i.key}
                            </a>
                          </td>
                          <td className="rp-title-cell">{i.title}</td>
                          <td className="muted">{i.type}</td>
                          <td className="rp-num">{daysText(i.cycleDays)}</td>
                          <td className="rp-num">{daysText(i.leadDays)}</td>
                          <td className="muted">{shortDate(i.resolvedAt)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </section>
            </>
          )
        }}
      </Loaded>
    </>
  )
}

// ---------- created vs resolved ----------

function CreatedReport({ project }: { project: Project }) {
  const [days, setDays] = useRange([14, 30, 90], 30)
  const q = useReport<CreatedResolved>(project.key, 'created-resolved', { days })
  return (
    <>
      <div className="toolbar">
        <RangeSwitch days={days} options={[14, 30, 90]} onChange={setDays} />
      </div>
      <Loaded q={q}>
        {(d) => {
          const rows = d.days
          const created = sum(rows.map((r) => r.created))
          const resolved = sum(rows.map((r) => r.resolved))
          if (!rows.length || created + resolved === 0)
            return (
              <Empty icon={<ChartColumn size={32} />} title={t('Nothing created or resolved in this period')}>
                {t('Try a longer time range.')}
              </Empty>
            )
          // more than a month: weekly columns (the last week ends today)
          const size = rows.length > 31 ? 7 : 1
          const buckets: (typeof rows)[] = []
          for (let end = rows.length; end > 0; end -= size) buckets.unshift(rows.slice(Math.max(0, end - size), end))
          const label = (b: typeof rows) => (b.length > 1 ? `${shortDate(b[0].date)} – ${shortDate(b[b.length - 1].date)}` : shortDate(b[0].date))
          let acc = 0
          const running = rows.map((r) => (acc += r.created - r.resolved))
          const net = created - resolved
          return (
            <>
              <div className="stat-row">
                <Tile value={fmt(created)} label={t('Created')} />
                <Tile value={fmt(resolved)} label={t('Resolved')} />
                <Tile value={signed(net)} label={net > 0 ? t('More created than resolved') : net < 0 ? t('More resolved than created') : t('Created and resolved balance out')} />
              </div>
              <ChartCard
                title={t('Created vs resolved')}
                subtitle={size > 1 ? t('Issues per week') : t('Issues per day')}
                legend={[
                  { label: t('Created'), color: SERIES[0] },
                  { label: t('Resolved'), color: SERIES[1] },
                ]}
                table={{
                  columns: [size > 1 ? t('Week') : t('Date'), t('Created'), t('Resolved')],
                  rows: buckets.map((b) => [label(b), fmt(sum(b.map((r) => r.created))), fmt(sum(b.map((r) => r.resolved)))]),
                  numeric: [1, 2],
                }}
              >
                <ColumnChart
                  summary={t('Created vs resolved over {count} days: {created} created, {resolved} resolved.', { count: days, created: fmt(created), resolved: fmt(resolved) })}
                  labels={buckets.map((b) => shortDate(b[0].date))}
                  series={[
                    { id: 'created', label: t('Created'), color: SERIES[0] },
                    { id: 'resolved', label: t('Resolved'), color: SERIES[1] },
                  ]}
                  values={buckets.map((b) => [sum(b.map((r) => r.created)), sum(b.map((r) => r.resolved))])}
                  integer
                  tip={(i) => {
                    const b = buckets[i]
                    const c = sum(b.map((r) => r.created))
                    const r = sum(b.map((x) => x.resolved))
                    return (
                      <TipBody
                        title={label(b)}
                        rows={[
                          { color: SERIES[0], value: fmt(c), label: t('Created') },
                          { color: SERIES[1], value: fmt(r), label: t('Resolved') },
                        ]}
                        note={t('Difference: {value}', { value: signed(c - r) })}
                      />
                    )
                  }}
                />
              </ChartCard>
              <ChartCard
                title={t('Running difference')}
                subtitle={t('Created minus resolved, added up since {date}; above zero the backlog grew', { date: shortDate(rows[0].date) })}
                table={{
                  columns: [t('Date'), t('Created'), t('Resolved'), t('Running difference')],
                  rows: rows.map((r, i) => [shortDate(r.date), fmt(r.created), fmt(r.resolved), signed(running[i])]),
                  numeric: [1, 2, 3],
                }}
              >
                <LineChart
                  summary={t('Running difference of created and resolved issues: {value} after {count} days.', { value: signed(running[running.length - 1]), count: days })}
                  height={200}
                  integer
                  series={[{ id: 'net', label: t('Running difference'), color: 'var(--rp-net)', wash: true, points: rows.map((r, i) => ({ x: dayMs(r.date), y: running[i] })) }]}
                  stops={rows.map((r) => dayMs(r.date))}
                  tip={(i) => (
                    <TipBody
                      title={dayLabel(dayMs(rows[i].date))}
                      rows={[{ color: 'var(--rp-net)', value: signed(running[i]), label: t('Running difference') }]}
                      note={t('{created} created, {resolved} resolved that day', { created: fmt(rows[i].created), resolved: fmt(rows[i].resolved) })}
                    />
                  )}
                />
              </ChartCard>
            </>
          )
        }}
      </Loaded>
    </>
  )
}

// ---------- workload ----------

function WorkloadReport({ project }: { project: Project }) {
  const q = useReport<Workload>(project.key, 'workload')
  return (
    <Loaded q={q}>
      {(d) => {
        const people = d.people
        if (!people.length)
          return (
            <Empty icon={<CircleCheck size={32} />} title={t('No open issues')}>
              {t('Everything in this project is done.')}
            </Empty>
          )
        const showPoints = people.some((p) => p.points > 0)
        const open = sum(people.map((p) => p.todo + p.inProgress))
        const unassigned = people.find((p) => p.id === null)
        const overdue = sum(people.map((p) => p.overdue))
        const rows: BarRow[] = people.map((p) => {
          const name = p.name ?? t('Unassigned')
          return {
            id: p.id ?? 'unassigned',
            name,
            label: (
              <span className="rp-name">
                {p.id ? <Avatar name={name} id={p.id} size={22} /> : <Unassigned size={22} />}
                <span>{name}</span>
              </span>
            ),
            segments: [
              { value: p.todo, color: CATEGORY_COLOR.todo, label: t('To do') },
              { value: p.inProgress, color: CATEGORY_COLOR.in_progress, label: t('In progress') },
            ],
            cells: [
              ...(showPoints ? [fmt(p.points)] : []),
              p.overdue > 0 ? (
                <span className="badge badge-danger">
                  <TriangleExclamationPoint size={12} aria-hidden />
                  {fmt(p.overdue)}
                </span>
              ) : (
                <span className="faint">0</span>
              ),
            ],
          }
        })
        return (
          <>
            <div className="stat-row">
              <Tile value={fmt(open)} label={t('Open issues')} />
              <Tile value={fmt(people.filter((p) => p.id !== null).length)} label={t('People with open work')} />
              <Tile value={fmt(unassigned ? unassigned.todo + unassigned.inProgress : 0)} label={t('Unassigned')} />
              <Tile value={fmt(overdue)} label={t('Overdue')} tone={overdue ? 'danger' : undefined} icon={<TriangleExclamationPoint size={18} aria-hidden />} />
            </div>
            <ChartCard
              title={t('Open work per person')}
              subtitle={showPoints ? t('Open issues by status category, with their story points and overdue issues') : t('Open issues by status category, with overdue issues')}
              legend={[
                { label: t('To do'), color: CATEGORY_COLOR.todo },
                { label: t('In progress'), color: CATEGORY_COLOR.in_progress },
              ]}
            >
              <BarList
                summary={t('Open work per person')}
                nameHeader={t('Person')}
                valueHeader={t('Open')}
                columns={[...(showPoints ? [t('Points')] : []), t('Overdue')]}
                rows={rows}
                shareOf={open}
              />
            </ChartCard>
          </>
        )
      }}
    </Loaded>
  )
}
