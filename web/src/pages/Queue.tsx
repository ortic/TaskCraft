// The service-level queue: open issues with goals, the most urgent first.
import { t } from '../i18n'
import { Link, useParams } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { Timer } from 'lucide-react'
import { api, type Issue } from '../api'
import { Avatar, Empty, ErrorBox, Loading, Lozenge, Unassigned, useProject } from '../ui'
import { PriorityIcon, TypeIcon } from '../icons'
import { useOpenIssue } from '../components/Layout'
import { SlaBadge, type SlaGoal } from '../components/SlaBadges'

interface QueueData {
  goals: Record<string, Record<string, number>> | null
  counts: { breached: number; atRisk: number; running: number }
  issues: { issue: Issue; sla: SlaGoal[] }[]
}

export default function Queue() {
  const { key } = useParams()
  const project = useProject(key)
  const q = useQuery({ queryKey: ['report', 'sla', key], queryFn: () => api.get<QueueData>(`/projects/${key}/sla`), refetchInterval: 60_000 })
  const open = useOpenIssue()
  if (project.isLoading || q.isLoading) return <Loading />
  if (q.error) return <div className="page"><ErrorBox error={q.error} /></div>
  const d = q.data!
  return (
    <div className="page">
      {project.data && <div className="crumbs"><Link to="/projects">{t('Projects')}</Link><span>/</span><Link to={`/projects/${project.data.key}`}>{project.data.name}</Link></div>}
      <div className="page-head">
        <h1>{t('Queue')}</h1>
        <span className="spacer" />
        {d.counts.breached > 0 && <span className="badge badge-danger">{t('{count} overdue', { count: d.counts.breached })}</span>}
        {d.counts.atRisk > 0 && <span className="badge badge-warning">{t('{count} at risk', { count: d.counts.atRisk })}</span>}
        <span className="badge">{t('{count} on track', { count: d.counts.running })}</span>
      </div>
      {!d.goals && (
        <Empty icon={<Timer size={32} />} title={t('No service levels set')}>
          {project.data?.role === 'admin' ? <Link to={`/projects/${key}/settings/sla`}>{t('Set goals in the project settings')}</Link> : t('A project admin can set them in the project settings.')}
        </Empty>
      )}
      {d.goals && d.issues.length === 0 && <Empty icon={<Timer size={32} />} title={t('Nothing waiting')}>{t('Every open issue has met its goals.')}</Empty>}
      {d.issues.length > 0 && (
        <div className="panel">
          {d.issues.map(({ issue: i, sla }) => (
            <div key={i.id} className="issue-row" style={{ height: 'auto', minHeight: 44, flexWrap: 'wrap', paddingBlock: 6 }} role="button" tabIndex={0} onClick={() => open(i.key)} onKeyDown={(e) => e.key === 'Enter' && open(i.key)}>
              <TypeIcon icon={i.type.icon} color={i.type.color} title={i.type.name} />
              <span className="issue-key" style={{ minWidth: 62 }}>{i.key}</span>
              <span className="ir-title">{i.title}</span>
              {sla.filter((g) => !g.stopped).map((g) => <SlaBadge key={g.goal} goal={g} />)}
              <PriorityIcon priority={i.priority} />
              <Lozenge name={i.status.name} category={i.status.category} />
              {i.assignee ? <Avatar name={i.assignee.name} id={i.assignee.id} size={22} /> : <Unassigned size={22} />}
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
