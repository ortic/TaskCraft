// Service-level goals of an issue: time left, missed by how much, or met.
import { t } from '../i18n'
import { Timer } from 'lucide-react'
import { duration } from '../api'

export interface SlaGoal {
  goal: 'response' | 'resolution'
  targetMinutes: number
  elapsedMinutes: number
  dueAt: string
  state: 'running' | 'atRisk' | 'breached' | 'met'
  stopped: boolean
}

export function goalLabel(g: SlaGoal['goal']): string {
  return g === 'response' ? t('First response') : t('Resolution')
}

/** 90 → "1h 30m"; 3000 → "2d 2h" (calendar days for long spans). */
function span(minutes: number): string {
  if (minutes < 48 * 60) return duration(minutes)
  const d = Math.floor(minutes / 1440)
  const h = Math.round((minutes % 1440) / 60)
  return h ? `${d}d ${h}h` : `${d}d`
}

export function slaText(g: SlaGoal): string {
  const left = g.targetMinutes - g.elapsedMinutes
  if (g.state === 'met') return t('met in {time}', { time: span(g.elapsedMinutes) })
  if (g.state === 'breached') return g.stopped ? t('missed by {time}', { time: span(-left) }) : t('overdue by {time}', { time: span(-left) })
  return t('{time} left', { time: span(left) })
}

const CLS: Record<SlaGoal['state'], string> = { running: 'badge', atRisk: 'badge badge-warning', breached: 'badge badge-danger', met: 'badge badge-success' }

export function SlaBadge({ goal, short }: { goal: SlaGoal; short?: boolean }) {
  return (
    <span className={CLS[goal.state]} title={`${goalLabel(goal.goal)}: ${slaText(goal)} · ${t('goal {time}', { time: span(goal.targetMinutes) })}`}>
      <Timer size={11} aria-hidden /> {short ? slaText(goal) : `${goalLabel(goal.goal)}: ${slaText(goal)}`}
    </span>
  )
}

export function SlaList({ goals }: { goals: SlaGoal[] }) {
  if (!goals.length) return null
  return (
    <div className="col" style={{ gap: 4, marginBottom: 10 }}>
      {goals.map((g) => <SlaBadge key={g.goal} goal={g} />)}
    </div>
  )
}
