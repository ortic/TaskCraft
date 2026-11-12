// Icons for issue types, priorities and projects (line icons; colour comes from the type).
import type { LucideIcon } from 'lucide-react'
import {
  Zap, Bookmark, SquareCheck, Bug, CornerDownRight, Lightbulb, CircleQuestionMark, Flag, Rocket, Globe, Server, LifeBuoy, Megaphone, Kanban, Package,
  Layers, Target, Building, Briefcase, Code, Palette, Wrench, Folder, Box, Cpu, Database, Cloud, Terminal, Leaf, Heart, Camera, Plane, GraduationCap, ShoppingCart,
  Milestone, ChevronsUp, ChevronUp, Equal, ChevronDown, Minus,
} from 'lucide-react'
import type { Priority } from './api'
import { t } from './i18n'

export const TYPE_ICONS: Record<string, LucideIcon> = {
  epic: Zap,
  story: Bookmark,
  task: SquareCheck,
  bug: Bug,
  subtask: CornerDownRight,
  lightbulb: Lightbulb,
  help: CircleQuestionMark,
  flag: Flag,
  milestone: Milestone,
  layers: Layers,
  target: Target,
  package: Package,
}

export const PROJECT_ICONS: Record<string, LucideIcon> = {
  rocket: Rocket,
  globe: Globe,
  server: Server,
  'life-buoy': LifeBuoy,
  megaphone: Megaphone,
  kanban: Kanban,
  building: Building,
  briefcase: Briefcase,
  code: Code,
  palette: Palette,
  wrench: Wrench,
  folder: Folder,
  box: Box,
  cpu: Cpu,
  database: Database,
  cloud: Cloud,
  terminal: Terminal,
  leaf: Leaf,
  heart: Heart,
  camera: Camera,
  plane: Plane,
  school: GraduationCap,
  shop: ShoppingCart,
  target: Target,
}

export const PROJECT_COLORS = ['#4f6bed', '#0e9f6e', '#d97706', '#e5484d', '#8b5cf6', '#0891b2', '#db2777', '#65a30d', '#475569', '#c2410c']

/** The type's icon on a small coloured tile. */
export function TypeIcon({ icon, color, size = 16, title }: { icon: string; color: string; size?: number; title?: string }) {
  const I = TYPE_ICONS[icon] ?? SquareCheck
  return (
    <span className="type-icon" style={{ background: color, width: size, height: size, borderRadius: Math.max(3, size / 4.5) }} title={title} aria-label={title} role={title ? 'img' : undefined}>
      <I size={Math.round(size * 0.68)} strokeWidth={2.4} color="#fff" aria-hidden />
    </span>
  )
}

export function ProjectIcon({ icon, color, size = 22, name }: { icon?: string | null; color?: string | null; size?: number; name?: string }) {
  const I = PROJECT_ICONS[icon ?? ''] ?? Rocket
  return (
    <span className="project-icon" style={{ background: color ?? '#4f6bed', width: size, height: size, borderRadius: Math.round(size * 0.26) }} title={name}>
      <I size={Math.round(size * 0.6)} color="#fff" strokeWidth={2.2} aria-hidden />
    </span>
  )
}

const PRIORITY_META: Record<Priority, { icon: LucideIcon; color: string; label: () => string }> = {
  urgent: { icon: ChevronsUp, color: 'var(--p-urgent)', label: () => t('Urgent') },
  high: { icon: ChevronUp, color: 'var(--p-high)', label: () => t('High') },
  medium: { icon: Equal, color: 'var(--p-medium)', label: () => t('Medium') },
  low: { icon: ChevronDown, color: 'var(--p-low)', label: () => t('Low') },
  none: { icon: Minus, color: 'var(--text-faint)', label: () => t('No priority') },
}

export function priorityLabel(p: Priority): string {
  return PRIORITY_META[p]?.label() ?? p
}

export function PriorityIcon({ priority, size = 16, withLabel = false }: { priority: Priority; size?: number; withLabel?: boolean }) {
  const m = PRIORITY_META[priority] ?? PRIORITY_META.medium
  const I = m.icon
  return (
    <span className="priority" title={m.label()} style={{ color: m.color }}>
      <I size={size} strokeWidth={2.6} aria-hidden />
      {withLabel ? <span className="priority-label">{m.label()}</span> : <span className="sr-only">{m.label()}</span>}
    </span>
  )
}
