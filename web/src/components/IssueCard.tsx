import { t } from '../i18n'
import { Ban, Calendar, MessageSquare, CornerDownRight } from 'lucide-react'
import type { Issue } from '../api'
import { Avatar, Unassigned } from '../ui'
import { PriorityIcon, TypeIcon } from '../icons'
import { DueText } from './fields'

/** Days in the current status, as dots: more dots and warmer colours the longer it sits. */
export function Aging({ since, done }: { since: string; done: boolean }) {
  if (done) return null
  const days = (Date.now() - new Date(since).getTime()) / 86400000
  if (days < 2) return null
  const n = days < 4 ? 1 : days < 7 ? 2 : days < 14 ? 3 : 4
  const cls = days >= 14 ? 'stale' : days >= 7 ? 'old' : ''
  return (
    <span className={`ic-age ${cls}`} title={t('{days} days in this status', { days: Math.floor(days) })} aria-label={t('{days} days in this status', { days: Math.floor(days) })}>
      {Array.from({ length: n }, (_, i) => <i key={i} />)}
    </span>
  )
}

export default function IssueCard({
  issue,
  fields,
  onOpen,
  draggable,
  onDragStart,
  onDragEnd,
  dragging,
  showEpic = true,
}: {
  issue: Issue
  fields: string[]
  onOpen: () => void
  draggable?: boolean
  onDragStart?: (e: React.DragEvent) => void
  onDragEnd?: () => void
  dragging?: boolean
  showEpic?: boolean
}) {
  const done = issue.status.category === 'done'
  const has = (f: string) => fields.includes(f)
  return (
    <div
      className={`issue-card ${done ? 'done' : ''} ${dragging ? 'dragging' : ''}`}
      role="button"
      tabIndex={0}
      draggable={draggable}
      onDragStart={onDragStart}
      onDragEnd={onDragEnd}
      onClick={onOpen}
      onKeyDown={(e) => e.key === 'Enter' && onOpen()}
      data-key={issue.key}
    >
      <div className="ic-title">{issue.title}</div>
      {(showEpic && issue.parent && issue.type.level === 0) || (has('labels') && issue.labels.length > 0) ? (
        <div className="row-wrap" style={{ gap: 4 }}>
          {showEpic && issue.parent && issue.type.level === 0 && (
            <span className="ic-epic" style={{ background: `color-mix(in srgb, ${issue.parent.color ?? '#8b5cf6'} 16%, transparent)`, color: 'var(--text)' }} title={issue.parent.title}>
              {issue.parent.title}
            </span>
          )}
          {has('labels') && issue.labels.slice(0, 3).map((l) => <span key={l} className="label-chip" style={{ height: 18, fontSize: 11 }}>{l}</span>)}
        </div>
      ) : null}
      <div className="ic-meta">
        <TypeIcon icon={issue.type.icon} color={issue.type.color} size={15} title={issue.type.name} />
        <span className="issue-key" style={{ textDecoration: done ? 'line-through' : undefined }}>{issue.key}</span>
        {issue.blocked && !done && <span className="ic-blocked" title={t('Blocked')}><Ban size={13} /></span>}
        {has('due') && issue.dueDate && (
          <span className="row" style={{ gap: 2 }}>
            <Calendar size={11} className="faint" />
            <DueText date={issue.dueDate} done={done} />
          </span>
        )}
        {issue.childCount > 0 && (
          <span className="ic-sub" title={t('Sub-tasks')}>
            <CornerDownRight size={11} />
            {issue.childDone}/{issue.childCount}
          </span>
        )}
        {issue.commentCount > 0 && has('comments') && (
          <span className="ic-sub"><MessageSquare size={11} />{issue.commentCount}</span>
        )}
        <Aging since={issue.statusChangedAt} done={done} />
        <span className="spacer" />
        {has('points') && issue.storyPoints != null && <span className="ic-points" title={t('Story points')}>{issue.storyPoints}</span>}
        {has('priority') && <PriorityIcon priority={issue.priority} size={15} />}
        {has('assignee') && (issue.assignee ? <Avatar name={issue.assignee.name} id={issue.assignee.id} size={22} /> : <Unassigned size={22} />)}
      </div>
    </div>
  )
}
