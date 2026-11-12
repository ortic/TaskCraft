// The portfolio: epics and releases of several projects on one timeline, and how loaded each
// team is across them.
import { t, locale } from '../i18n'
import { useMemo, useState } from 'react'
import { Link, useSearchParams } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { Briefcase, Package, ChevronLeft, ChevronRight } from 'lucide-react'
import { api, qs, shortDate } from '../api'
import { Avatar, Empty, ErrorBox, Loading, PopButton, useProjects } from '../ui'
import { ProjectIcon } from '../icons'
import { useOpenIssue } from '../components/Layout'

interface PEpic {
  id: string
  key: string
  title: string
  color: string | null
  startDate: string | null
  dueDate: string | null
  category: string
  childCount: number
  childDone: number
  points: number
  pointsDone: number
  childStart: string | null
  childEnd: string | null
}
interface PVersion {
  id: string
  name: string
  status: string
  startDate: string | null
  releaseDate: string | null
  total: number
  done: number
}
interface PProject {
  id: string
  key: string
  name: string
  color: string | null
  icon: string | null
  kind: string
  epics: PEpic[]
  versions: PVersion[]
}
interface PMember {
  id: string
  name: string
  todo: number
  inProgress: number
  points: number
  overdue: number
  dueThisWeek: number
}
interface PData {
  projects: PProject[]
  teams: { id: string; name: string; members: PMember[] }[]
}

const DAY = 86400000
const day = (s: string) => new Date(s + 'T00:00:00').getTime()

export default function Portfolio() {
  const [params, setParams] = useSearchParams()
  const selected = params.get('projects') ?? ''
  const all = useProjects()
  const q = useQuery({ queryKey: ['report', 'portfolio', selected], queryFn: () => api.get<PData>(`/portfolio${qs({ projects: selected })}`) })
  const open = useOpenIssue()
  const [offset, setOffset] = useState(0)
  // six months from last month, moved by the arrows
  const start = useMemo(() => {
    const d = new Date()
    d.setDate(1)
    d.setHours(0, 0, 0, 0)
    d.setMonth(d.getMonth() - 1 + offset)
    return d.getTime()
  }, [offset])
  const end = useMemo(() => {
    const d = new Date(start)
    d.setMonth(d.getMonth() + 6)
    return d.getTime()
  }, [start])
  const months = useMemo(() => {
    const out: { label: string; at: number }[] = []
    const d = new Date(start)
    while (d.getTime() < end) {
      out.push({ label: new Intl.DateTimeFormat(locale(), { month: 'short', year: d.getMonth() === 0 ? 'numeric' : undefined }).format(d), at: d.getTime() })
      d.setMonth(d.getMonth() + 1)
    }
    return out
  }, [start, end])
  const pos = (ms: number) => Math.max(0, Math.min(100, ((ms - start) / (end - start)) * 100))
  const today = pos(Date.now())
  const shown = selected ? selected.split(',') : (q.data?.projects ?? []).map((p) => p.key)
  const toggle = (key: string) => {
    const next = shown.includes(key) ? shown.filter((k) => k !== key) : [...shown, key]
    const p = new URLSearchParams(params)
    if (next.length) p.set('projects', next.join(','))
    else p.delete('projects')
    setParams(p, { replace: true })
  }
  return (
    <div className="page-wide">
      <div className="page-head">
        <h1><Briefcase size={22} /> {t('Portfolio')}</h1>
        <span className="spacer" />
        <PopButton className="btn btn-sm" label={t('Projects ({count})', { count: shown.length })} width={280} align="end">
          {() => (
            <div className="choices-list">
              {(all.data ?? []).filter((p) => !p.archivedAt).map((p) => (
                <label key={p.id} className="menu-item">
                  <input type="checkbox" checked={shown.includes(p.key)} onChange={() => toggle(p.key)} />
                  <ProjectIcon icon={p.icon} color={p.color} size={18} /> {p.name}
                </label>
              ))}
            </div>
          )}
        </PopButton>
        <div className="seg" role="group" aria-label={t('Time range')}>
          <button onClick={() => setOffset((o) => o - 3)} aria-label={t('Earlier')}><ChevronLeft size={15} /></button>
          <button onClick={() => setOffset(0)}>{t('Now')}</button>
          <button onClick={() => setOffset((o) => o + 3)} aria-label={t('Later')}><ChevronRight size={15} /></button>
        </div>
      </div>
      <p className="muted small" style={{ marginTop: -8 }}>{t('Epics (bars, filled as their issues get done) and releases (diamonds) of your starred projects, or the ones you pick.')}</p>
      {q.isLoading && <Loading />}
      {q.error && <ErrorBox error={q.error} />}
      {q.data && q.data.projects.length === 0 && <Empty icon={<Briefcase size={32} />} title={t('No projects to show')} />}
      {q.data && q.data.projects.length > 0 && (
        <div className="pf-chart" role="table" aria-label={t('Portfolio timeline')}>
          <div className="pf-row pf-head" role="row">
            <div className="pf-label" role="columnheader">{t('Project / epic')}</div>
            <div className="pf-track" role="columnheader">
              {months.map((m) => <span key={m.at} className="pf-month" style={{ left: `${pos(m.at)}%` }}>{m.label}</span>)}
            </div>
          </div>
          {q.data.projects.map((p) => (
            <div key={p.id} role="rowgroup">
              <div className="pf-row pf-project" role="row">
                <div className="pf-label" role="rowheader">
                  <Link to={`/projects/${p.key}`} className="row" style={{ gap: 8, color: 'var(--text)', fontWeight: 650 }}>
                    <ProjectIcon icon={p.icon} color={p.color} size={20} /> {p.name}
                  </Link>
                </div>
                <div className="pf-track" role="cell">
                  {today > 0 && today < 100 && <span className="pf-today" style={{ left: `${today}%` }} />}
                  {p.versions.filter((v) => v.releaseDate).map((v) => {
                    const x = pos(day(v.releaseDate!))
                    if (x <= 0 || x >= 100) return null
                    const late = v.status !== 'released' && day(v.releaseDate!) < Date.now() - DAY
                    return (
                      <span key={v.id} className={`pf-release ${v.status === 'released' ? 'done' : late ? 'late' : ''}`} style={{ left: `${x}%` }} title={`${v.name} · ${shortDate(v.releaseDate)} · ${t('{done} of {total} done', { done: v.done, total: v.total })}`}>
                        <Package size={10} aria-hidden />
                        <span className="pf-release-name">{v.name}</span>
                      </span>
                    )
                  })}
                </div>
              </div>
              {p.epics.map((e) => {
                const s = e.startDate ?? e.childStart
                const f = e.dueDate ?? e.childEnd
                const pct = e.points > 0 ? (e.pointsDone / e.points) * 100 : e.childCount ? (e.childDone / e.childCount) * 100 : e.category === 'done' ? 100 : 0
                const left = s ? pos(day(s)) : null
                const right = f ? pos(day(f) + DAY) : null
                const late = e.category !== 'done' && e.dueDate && day(e.dueDate) < Date.now() - DAY
                return (
                  <div key={e.id} className="pf-row" role="row">
                    <div className="pf-label" role="rowheader">
                      <button className="pf-epic-name" onClick={() => open(e.key)} title={e.title}>
                        <span className="dot" style={{ background: e.color ?? 'var(--accent)' }} />
                        <span className="issue-key">{e.key}</span> {e.title}
                      </button>
                    </div>
                    <div className="pf-track" role="cell">
                      {today > 0 && today < 100 && <span className="pf-today" style={{ left: `${today}%` }} />}
                      {left != null && right != null && right > left ? (
                        <button
                          className={`pf-bar ${late ? 'late' : ''} ${e.startDate && e.dueDate ? '' : 'derived'}`}
                          style={{ left: `${left}%`, width: `${Math.max(1, right - left)}%`, ['--pf-c' as string]: e.color ?? 'var(--accent)' }}
                          onClick={() => open(e.key)}
                          title={`${e.title} · ${shortDate(s)} – ${shortDate(f)} · ${Math.round(pct)}%`}
                          aria-label={t('{title}: {start} to {end}, {pct}% done', { title: e.title, start: shortDate(s), end: shortDate(f), pct: Math.round(pct) })}
                        >
                          <span className="pf-fill" style={{ width: `${pct}%` }} />
                          {right - left >= 4 && <span className="pf-pct">{Math.round(pct)}%</span>}
                        </button>
                      ) : (
                        <span className="faint small pf-nodates">{t('no dates')} · {t('{done} of {total} done', { done: e.childDone, total: e.childCount })}</span>
                      )}
                    </div>
                  </div>
                )
              })}
              {p.epics.length === 0 && (
                <div className="pf-row" role="row">
                  <div className="pf-label faint small" role="rowheader">{t('No open epics')}</div>
                  <div className="pf-track" role="cell" />
                </div>
              )}
            </div>
          ))}
        </div>
      )}
      {q.data && q.data.teams.length > 0 && (
        <section style={{ marginTop: 28 }}>
          <h2>{t('Team load')}</h2>
          <p className="muted small">{t('Open issues assigned to each team member in these projects.')}</p>
          <div className="grid-cards" style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(360px, 1fr))' }}>
            {q.data.teams.map((team) => {
              const max = Math.max(1, ...team.members.map((m) => m.todo + m.inProgress))
              return (
                <div key={team.id} className="panel">
                  <div className="panel-head"><h2>{team.name}</h2><span className="faint small">{t('{count} people', { count: team.members.length })}</span></div>
                  <table className="list">
                    <thead>
                      <tr>
                        <th>{t('Person')}</th>
                        <th>{t('Open')}</th>
                        <th style={{ textAlign: 'end' }}>{t('Points')}</th>
                        <th style={{ textAlign: 'end' }}>{t('Overdue')}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {team.members.map((m) => (
                        <tr key={m.id}>
                          <td><span className="row" style={{ gap: 6 }}><Avatar name={m.name} id={m.id} size={20} decorative />{m.name}</span></td>
                          <td style={{ width: '40%' }}>
                            <div className="progress" style={{ height: 8 }} title={t('{todo} to do, {doing} in progress', { todo: m.todo, doing: m.inProgress })}>
                              <span className="p-progress" style={{ width: `${(m.inProgress / max) * 100}%` }} />
                              <span style={{ width: `${(m.todo / max) * 100}%`, background: 'var(--border-strong)' }} />
                            </div>
                            <span className="faint small">{t('{todo} to do · {doing} in progress', { todo: m.todo, doing: m.inProgress })}</span>
                          </td>
                          <td style={{ textAlign: 'end' }}>{m.points || ''}</td>
                          <td style={{ textAlign: 'end', color: m.overdue ? 'var(--danger)' : undefined }}>{m.overdue || ''}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )
            })}
          </div>
        </section>
      )}
    </div>
  )
}
