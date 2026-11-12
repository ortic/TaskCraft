import { t } from '../i18n'
import { useState } from 'react'
import { Link } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { CircleCheck, Inbox, Star } from 'lucide-react'
import { api, ago, duration, type HistoryEvent, type Issue } from '../api'
import { Avatar, BotAvatar, Empty, Loading, Lozenge, Tabs, useMe, useProjects } from '../ui'
import { PriorityIcon, ProjectIcon, TypeIcon } from '../icons'
import { DueText } from '../components/fields'
import { useOpenIssue } from '../components/Layout'
import { describeEvent } from '../components/IssueView'

interface MyWork {
  assigned: Issue[]
  reported: Issue[]
  doneThisWeek: Issue[]
  watching: Issue[]
  recent: Issue[]
  loggedThisWeek: number
}

export function IssueLine({ issue, showProject = true }: { issue: Issue; showProject?: boolean }) {
  const open = useOpenIssue()
  const done = issue.status.category === 'done'
  return (
    <div className="issue-row" onClick={() => open(issue.key)} role="button" tabIndex={0} onKeyDown={(e) => e.key === 'Enter' && open(issue.key)}>
      <TypeIcon icon={issue.type.icon} color={issue.type.color} title={issue.type.name} />
      <span className="issue-key" style={{ minWidth: 64 }}>{issue.key}</span>
      <span className="ir-title" style={{ textDecoration: done ? 'line-through' : undefined }}>{issue.title}</span>
      {showProject && <span className="faint small" style={{ whiteSpace: 'nowrap' }}>{issue.projectName}</span>}
      {issue.dueDate && <DueText date={issue.dueDate} done={done} />}
      <PriorityIcon priority={issue.priority} />
      <Lozenge name={issue.status.name} category={issue.status.category} />
    </div>
  )
}

export default function Home() {
  const me = useMe().data
  const work = useQuery({ queryKey: ['my-work'], queryFn: () => api.get<MyWork>('/my-work') })
  const activity = useQuery({ queryKey: ['activity'], queryFn: () => api.get<HistoryEvent[]>('/activity') })
  const projects = useProjects()
  const [tab, setTab] = useState<'assigned' | 'recent' | 'watching' | 'reported' | 'done'>('assigned')
  const open = useOpenIssue()
  const w = work.data
  const hour = new Date().getHours()
  const greeting = hour < 11 ? t('Good morning, {name}') : hour < 18 ? t('Hello, {name}') : t('Good evening, {name}')
  const firstName = me?.user?.name.split(' ')[0] ?? ''
  const overdue = (w?.assigned ?? []).filter((i) => i.dueDate && new Date(i.dueDate + 'T23:59:59') < new Date()).length
  const weekEnd = new Date()
  weekEnd.setDate(weekEnd.getDate() + ((7 - weekEnd.getDay()) % 7))
  const dueWeek = (w?.assigned ?? []).filter((i) => i.dueDate && new Date(i.dueDate) <= weekEnd).length
  const list = w ? (tab === 'done' ? w.doneThisWeek : w[tab]) : []
  const inProgress = list.filter((i) => i.status.category === 'in_progress')
  const rest = list.filter((i) => i.status.category !== 'in_progress')
  const starred = (projects.data ?? []).filter((p) => p.starred && !p.archivedAt)

  return (
    <div className="page">
      <div className="page-head">
        <h1>{greeting.replace('{name}', firstName)}</h1>
      </div>
      <div className="stat-row">
        <Stat value={w?.assigned.length} label={t('Open issues assigned to you')} />
        <Stat value={dueWeek} label={t('Due this week')} />
        <Stat value={overdue} label={t('Overdue')} tone={overdue ? 'danger' : undefined} />
        <Stat value={w?.doneThisWeek.length} label={t('Finished in the last 7 days')} tone="success" />
        <Stat value={w ? duration(w.loggedThisWeek) : undefined} label={t('Time logged this week')} />
      </div>
      <div className="home-grid">
        <section className="panel">
          <div style={{ padding: '6px 14px 0' }}>
            <Tabs
              value={tab}
              onChange={setTab}
              tabs={[
                { id: 'assigned', label: t('Assigned to me') },
                { id: 'recent', label: t('Recently viewed') },
                { id: 'watching', label: t('Watching') },
                { id: 'reported', label: t('Reported by me') },
                { id: 'done', label: t('Done') },
              ]}
            />
          </div>
          {work.isLoading && <Loading />}
          {w && list.length === 0 && (
            <Empty icon={tab === 'assigned' ? <CircleCheck size={32} /> : <Inbox size={32} />} title={tab === 'assigned' ? t('Nothing assigned to you') : t('Nothing here yet')}>
              {tab === 'assigned' && t('Issues assigned to you show up here, the ones in progress first.')}
            </Empty>
          )}
          {tab === 'assigned' && inProgress.length > 0 && <div className="menu-head" style={{ padding: '8px 14px 4px' }}>{t('In progress')}</div>}
          {(tab === 'assigned' ? inProgress : []).map((i) => <IssueLine key={i.id} issue={i} />)}
          {tab === 'assigned' && rest.length > 0 && inProgress.length > 0 && <div className="menu-head" style={{ padding: '12px 14px 4px' }}>{t('Up next')}</div>}
          {(tab === 'assigned' ? rest : list).map((i) => <IssueLine key={i.id} issue={i} />)}
        </section>
        <div className="col" style={{ gap: 16 }}>
          <section className="panel">
            <div className="panel-head"><h2><Star size={15} /> {t('Starred projects')}</h2><span className="spacer" /><Link to="/projects" className="small">{t('All projects')}</Link></div>
            <div className="panel-body">
              {starred.length === 0 && <div className="faint small" style={{ padding: '8px 14px' }}>{t('Star projects to keep them here and in the sidebar.')}</div>}
              {starred.map((p) => (
                <Link key={p.id} to={`/projects/${p.key}`} className="menu-item" style={{ padding: '6px 14px', borderRadius: 0 }}>
                  <ProjectIcon icon={p.icon} color={p.color} size={22} />
                  <span style={{ flex: 1, minWidth: 0 }}>
                    <div>{p.name}</div>
                    <div className="mi-desc">{p.activeSprint ? p.activeSprint : t('{count} open', { count: p.openCount ?? 0 })}</div>
                  </span>
                </Link>
              ))}
            </div>
          </section>
          <section className="panel">
            <div className="panel-head"><h2>{t('Activity')}</h2></div>
            <div className="panel-body" style={{ maxHeight: 520, overflowY: 'auto', padding: '4px 14px' }}>
              {(activity.data ?? []).slice(0, 30).map((e) => (
                <div key={e.id} className="activity-item small">
                  {e.actor ? <Avatar name={e.actor.name} id={e.actor.id} size={22} /> : <BotAvatar size={22} />}
                  <div className="ai-body">
                    <strong>{e.actor?.name ?? t('Automation')}</strong> {describeEvent(e)}{' '}
                    <a href={`?issue=${e.key}`} onClick={(ev) => { ev.preventDefault(); open(e.key!) }}><span className="issue-key">{e.key}</span></a>
                    <div className="faint" style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{e.title} · {ago(e.at)}</div>
                  </div>
                </div>
              ))}
            </div>
          </section>
        </div>
      </div>
    </div>
  )
}

function Stat({ value, label, tone }: { value: number | string | undefined; label: string; tone?: 'danger' | 'success' }) {
  return (
    <div className="stat">
      <div className="stat-value" style={{ color: tone === 'danger' ? 'var(--danger)' : tone === 'success' ? 'var(--success)' : undefined }}>{value ?? '–'}</div>
      <div className="stat-label">{label}</div>
    </div>
  )
}
