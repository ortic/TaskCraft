// The time report: logged time in a period, grouped by person, project, issue, category or
// period (and once more inside), with filters and a CSV download of every log.
import { t, locale } from '../i18n'
import { Fragment, useState } from 'react'
import { Link, useSearchParams } from 'react-router-dom'
import { keepPreviousData, useQuery } from '@tanstack/react-query'
import { ChevronDown, ChevronRight, Download, Timer } from 'lucide-react'
import { api, duration, isoDate, qs } from '../api'
import { Empty, ErrorBox, Loading, useMe, usePeople, useProjects } from '../ui'
import { useOpenIssue } from '../components/Layout'
import { useTimeCategories } from '../components/IssueView'
import './Timesheet.css'

interface Row {
  id: string
  label: string
  minutes: number
  billableMinutes: number
  children?: Row[]
}

interface Report {
  from: string
  to: string
  group: string
  then: string | null
  totalMinutes: number
  billableMinutes: number
  logCount: number
  truncated: boolean
  rows: Row[]
}

const GROUPS = ['person', 'project', 'issue', 'category', 'day', 'week', 'month'] as const
type Group = (typeof GROUPS)[number]

function groupLabel(g: Group): string {
  switch (g) {
    case 'person':
      return t('Person')
    case 'project':
      return t('Project')
    case 'issue':
      return t('Issue')
    case 'category':
      return t('Category')
    case 'day':
      return t('Day')
    case 'week':
      return t('Week')
    case 'month':
      return t('Month')
  }
}

type Period = 'this-week' | 'last-week' | 'this-month' | 'last-month' | 'this-year' | 'custom'

function periodRange(p: Period, weekStartsSunday: boolean): [string, string] {
  const now = new Date()
  const day = (d: Date, n: number) => new Date(d.getFullYear(), d.getMonth(), d.getDate() + n)
  const monday = day(now, -((now.getDay() - (weekStartsSunday ? 0 : 1) + 7) % 7))
  switch (p) {
    case 'this-week':
      return [isoDate(monday), isoDate(day(monday, 6))]
    case 'last-week':
      return [isoDate(day(monday, -7)), isoDate(day(monday, -1))]
    case 'last-month':
      return [isoDate(new Date(now.getFullYear(), now.getMonth() - 1, 1)), isoDate(new Date(now.getFullYear(), now.getMonth(), 0))]
    case 'this-year':
      return [isoDate(new Date(now.getFullYear(), 0, 1)), isoDate(new Date(now.getFullYear(), 11, 31))]
    default:
      return [isoDate(new Date(now.getFullYear(), now.getMonth(), 1)), isoDate(new Date(now.getFullYear(), now.getMonth() + 1, 0))]
  }
}

export default function TimeReport() {
  const me = useMe().data
  const people = usePeople().data ?? []
  const projects = useProjects().data ?? []
  const categories = useTimeCategories().data ?? []
  const open = useOpenIssue()
  const [params, setParams] = useSearchParams()
  const get = (k: string, d = '') => params.get(k) ?? d
  const set = (patch: Record<string, string | null>) =>
    setParams((p) => {
      const n = new URLSearchParams(p)
      for (const [k, v] of Object.entries(patch)) {
        if (v) n.set(k, v)
        else n.delete(k)
      }
      return n
    })
  const period = get('period', 'this-month') as Period
  const sunday = me?.workspace?.weekStart === 'sunday'
  const [from, to] = period === 'custom' ? [get('from', periodRange('this-month', sunday)[0]), get('to', periodRange('this-month', sunday)[1])] : periodRange(period, sunday)
  const group = get('group', 'person') as Group
  const then = get('then', group === 'issue' ? '' : 'issue')
  const filters = { from, to, group, then: then || null, projects: get('project') || null, user: get('user') || null, category: get('category') || null, billable: get('billable') || null }
  const report = useQuery({
    queryKey: ['time-report', filters],
    queryFn: () => api.get<Report>(`/time-report${qs(filters)}`),
    placeholderData: keepPreviousData,
  })
  const [openRows, setOpenRows] = useState<Record<string, boolean>>({})
  const r = report.data
  const max = Math.max(1, ...(r?.rows ?? []).map((x) => x.minutes))
  const csv = `/api/time-report${qs({ ...filters, format: 'csv' })}`
  const rangeFmt = new Intl.DateTimeFormat(locale(), { day: 'numeric', month: 'short', year: 'numeric' })
  const label = (g: string, row: Row) => {
    if (g === 'person' && !row.label) return <span className="faint">{t('(someone removed)')}</span>
    if (g === 'category' && !row.label) return <span className="faint">{t('No category')}</span>
    if (g === 'issue') {
      const key = row.label.split(' ')[0]
      return (
        <a href={`/issues/${key}`} onClick={(e) => { e.preventDefault(); open(key) }}>
          <span className="issue-key">{key}</span> {row.label.slice(key.length + 1)}
        </a>
      )
    }
    if (g === 'day') return new Date(`${row.label}T00:00`).toLocaleDateString(locale(), { weekday: 'short', day: 'numeric', month: 'short', year: 'numeric' })
    if (g === 'week') return t('Week of {date}', { date: new Date(`${row.label}T00:00`).toLocaleDateString(locale(), { day: 'numeric', month: 'short', year: 'numeric' }) })
    if (g === 'month') return new Date(`${row.label}-01T00:00`).toLocaleDateString(locale(), { month: 'long', year: 'numeric' })
    return row.label
  }

  return (
    <div className="page tr-page">
      <div className="page-head">
        <h1><Timer size={20} /> {t('Time report')}</h1>
        <span className="spacer" />
        <Link className="btn" to="/timesheet">{t('My timesheet')}</Link>
        <a className="btn" href={csv} download><Download size={15} /> {t('Download CSV')}</a>
      </div>
      <div className="tr-filters">
        <label className="field">
          <span className="field-label">{t('Period')}</span>
          <select className="select input-sm" value={period} onChange={(e) => set({ period: e.target.value === 'this-month' ? null : e.target.value, ...(e.target.value === 'custom' ? { from, to } : { from: null, to: null }) })}>
            <option value="this-week">{t('This week')}</option>
            <option value="last-week">{t('Last week')}</option>
            <option value="this-month">{t('This month')}</option>
            <option value="last-month">{t('Last month')}</option>
            <option value="this-year">{t('This year')}</option>
            <option value="custom">{t('Custom')}</option>
          </select>
        </label>
        {period === 'custom' && (
          <>
            <label className="field">
              <span className="field-label">{t('From')}</span>
              <input type="date" className="input input-sm" value={from} onChange={(e) => e.target.value && set({ from: e.target.value })} />
            </label>
            <label className="field">
              <span className="field-label">{t('To')}</span>
              <input type="date" className="input input-sm" value={to} onChange={(e) => e.target.value && set({ to: e.target.value })} />
            </label>
          </>
        )}
        <label className="field">
          <span className="field-label">{t('Group by')}</span>
          <select className="select input-sm" value={group} onChange={(e) => set({ group: e.target.value === 'person' ? null : e.target.value, then: e.target.value === then ? 'none' : then || null })}>
            {GROUPS.map((g) => <option key={g} value={g}>{groupLabel(g)}</option>)}
          </select>
        </label>
        <label className="field">
          <span className="field-label">{t('Then by')}</span>
          <select className="select input-sm" value={then || 'none'} onChange={(e) => set({ then: e.target.value === 'issue' ? null : e.target.value })}>
            <option value="none">{t('Nothing')}</option>
            {GROUPS.filter((g) => g !== group).map((g) => <option key={g} value={g}>{groupLabel(g)}</option>)}
          </select>
        </label>
        <label className="field">
          <span className="field-label">{t('Project')}</span>
          <select className="select input-sm" value={get('project')} onChange={(e) => set({ project: e.target.value || null })}>
            <option value="">{t('All projects')}</option>
            {projects.map((p) => <option key={p.id} value={p.key}>{p.name}</option>)}
          </select>
        </label>
        <label className="field">
          <span className="field-label">{t('Person')}</span>
          <select className="select input-sm" value={get('user')} onChange={(e) => set({ user: e.target.value || null })}>
            <option value="">{t('Everyone')}</option>
            {people.map((p) => <option key={p.id} value={p.id}>{p.name}</option>)}
          </select>
        </label>
        {categories.length > 0 && (
          <label className="field">
            <span className="field-label">{t('Category')}</span>
            <select className="select input-sm" value={get('category')} onChange={(e) => set({ category: e.target.value || null })}>
              <option value="">{t('All categories')}</option>
              <option value="none">{t('No category')}</option>
              {categories.map((c) => <option key={c.id} value={c.id}>{c.name}</option>)}
            </select>
          </label>
        )}
        <label className="field">
          <span className="field-label">{t('Billable')}</span>
          <select className="select input-sm" value={get('billable')} onChange={(e) => set({ billable: e.target.value || null })}>
            <option value="">{t('All time')}</option>
            <option value="true">{t('Billable only')}</option>
            <option value="false">{t('Not billable only')}</option>
          </select>
        </label>
      </div>
      {report.error && <ErrorBox error={report.error} />}
      {!r && report.isLoading && <Loading />}
      {r && (
        <>
          <div className="tr-summary">
            <div><span className="tr-big">{duration(r.totalMinutes)}</span> <span className="faint">{t('logged')}</span></div>
            <div><span className="tr-big">{duration(r.billableMinutes)}</span> <span className="faint">{t('billable')}</span></div>
            <div className="faint small">{rangeFmt.formatRange(new Date(`${r.from}T00:00`), new Date(`${r.to}T00:00`))} · {r.logCount === 1 ? t('1 time log') : t('{n} time logs', { n: r.logCount })}</div>
          </div>
          {r.truncated && <p className="error-text small">{t('Only the first 100,000 time logs are counted: choose a shorter period.')}</p>}
          {r.rows.length === 0 ? (
            <Empty icon={<Timer size={32} />} title={t('No time logged in this period')} />
          ) : (
            <table className="list tr-table">
              <thead>
                <tr>
                  <th>{groupLabel(group)}</th>
                  <th className="tr-num">{t('Hours')}</th>
                  <th className="tr-num">{t('Billable')}</th>
                  <th className="tr-bar-col"><span className="sr-only">{t('Share')}</span></th>
                </tr>
              </thead>
              <tbody>
                {r.rows.map((row) => {
                  const key = row.id || 'none'
                  const expanded = openRows[key] ?? r.rows.length <= 5
                  return (
                    <Fragment key={key}>
                      <tr className="tr-row">
                        <td>
                          <span className="row" style={{ gap: 4 }}>
                            {row.children ? (
                              <button className="icon-btn icon-btn-sm" aria-expanded={expanded} aria-label={expanded ? t('Collapse') : t('Expand')} onClick={() => setOpenRows((o) => ({ ...o, [key]: !expanded }))}>
                                {expanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                              </button>
                            ) : null}
                            <strong>{label(group, row)}</strong>
                          </span>
                        </td>
                        <td className="tr-num"><strong>{duration(row.minutes)}</strong></td>
                        <td className="tr-num faint">{row.billableMinutes ? duration(row.billableMinutes) : '—'}</td>
                        <td className="tr-bar-col">
                          <div className="progress" title={`${Math.round((row.minutes / Math.max(1, r.totalMinutes)) * 100)}%`}>
                            <span style={{ width: `${(row.minutes / max) * 100}%`, background: 'var(--accent)' }} />
                          </div>
                        </td>
                      </tr>
                      {expanded &&
                        row.children?.map((c) => (
                          <tr key={`${key}/${c.id || 'none'}`} className="tr-child">
                            <td>{label(r.then ?? '', c)}</td>
                            <td className="tr-num">{duration(c.minutes)}</td>
                            <td className="tr-num faint">{c.billableMinutes ? duration(c.billableMinutes) : '—'}</td>
                            <td className="tr-bar-col" />
                          </tr>
                        ))}
                    </Fragment>
                  )
                })}
              </tbody>
            </table>
          )}
        </>
      )}
    </div>
  )
}
