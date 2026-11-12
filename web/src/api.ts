// Typed client for the TaskCraft REST API.
import { locale } from './i18n'

export class ApiError extends Error {
  status: number
  code: string
  constructor(status: number, code: string, message: string) {
    super(message)
    this.status = status
    this.code = code
  }
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { 'x-taskcraft': '1' }
  let payload: BodyInit | undefined
  if (body instanceof FormData) {
    payload = body
  } else if (body !== undefined) {
    headers['content-type'] = 'application/json'
    payload = JSON.stringify(body)
  }
  const res = await fetch(`/api${path}`, { method, headers, body: payload, credentials: 'same-origin' })
  if (res.status === 204) return undefined as T
  const text = await res.text()
  let data: any
  try {
    data = text ? JSON.parse(text) : undefined
  } catch {
    data = undefined
  }
  if (!res.ok) throw new ApiError(res.status, data?.error ?? 'error', data?.message ?? res.statusText)
  return data as T
}

export const api = {
  get: <T>(p: string) => request<T>('GET', p),
  post: <T>(p: string, b?: unknown) => request<T>('POST', p, b ?? {}),
  put: <T>(p: string, b?: unknown) => request<T>('PUT', p, b ?? {}),
  patch: <T>(p: string, b?: unknown) => request<T>('PATCH', p, b ?? {}),
  del: <T>(p: string, b?: unknown) => request<T>('DELETE', p, b),
  upload: <T>(p: string, form: FormData) => request<T>('POST', p, form),
}

export const qs = (o: Record<string, string | number | boolean | null | undefined>) => {
  const p = new URLSearchParams()
  for (const [k, v] of Object.entries(o)) if (v !== undefined && v !== null && v !== '') p.set(k, String(v))
  const s = p.toString()
  return s ? `?${s}` : ''
}

// ---- types (mirror the server's camelCase JSON) ----

export type Category = 'todo' | 'in_progress' | 'done'
export type Priority = 'none' | 'low' | 'medium' | 'high' | 'urgent'
export type Role = 'none' | 'viewer' | 'member' | 'admin'
export const PRIORITIES: Priority[] = ['urgent', 'high', 'medium', 'low', 'none']

export interface User {
  id: string
  workspaceId: string
  email: string | null
  name: string
  role: 'owner' | 'admin' | 'member' | 'guest'
  status: string
}

export interface Me {
  user: User | null
  profile?: { title: string | null; locale: string | null; timezone: string | null }
  preferences?: Record<string, any>
  workspace: { id: string; name: string; accentColor?: string | null; defaultLanguage?: string | null; weekStart?: string | null } | null
  wikicraft?: { url: string | null; connected: boolean; personal: boolean }
  baseUrl?: string
  /** the workspace requires two-factor authentication and this person hasn't set it up */
  mustSetUpTwoFactor?: boolean
}

export interface Named {
  id: string
  name: string
}

export interface Person extends Named {
  email?: string | null
  role?: string
  title?: string | null
}

export interface StatusRef {
  id: string
  name: string
  category: Category
  color: string | null
}

export interface TypeRef {
  id: string
  name: string
  icon: string
  color: string
  /** 1 epic, 0 standard, -1 sub-task */
  level: number
}

export interface Issue {
  id: string
  key: string
  number: number
  projectId: string
  projectKey: string
  projectName: string
  title: string
  type: TypeRef
  status: StatusRef
  priority: Priority
  assignee: Named | null
  reporter: Named | null
  parent: { id: string; key: string; title: string; color: string | null } | null
  sprint: { id: string; name: string; state: string } | null
  /** the releases it's fixed in */
  versions: Named[]
  /** the releases it was found in */
  affectedVersions: Named[]
  components: Named[]
  storyPoints: number | null
  /** the original estimate */
  estimateMinutes: number | null
  /** what's left: set by someone, else the original estimate minus the time logged */
  remainingMinutes?: number | null
  /** someone set the remaining estimate (else it follows the time logged) */
  remainingSet?: boolean
  spentMinutes: number
  startDate: string | null
  dueDate: string | null
  rank: number
  labels: string[]
  custom: Record<string, any>
  resolvedAt: string | null
  /** why a done issue is done (Done, Won't do, …) */
  resolution?: string | null
  statusChangedAt: string
  createdAt: string
  updatedAt: string
  /** a private issue's security level: who sees it besides its reporter, creator and assignee */
  security?: Named | null
  childCount: number
  childDone: number
  commentCount: number
  /** its pull request's state (Git integration), when one is linked */
  pullRequest?: 'open' | 'merged' | 'closed'
  blocked: boolean
  url: string
}

/** What time is spent on (workspace-wide), and whether such time is billable by default. */
export interface TimeCategory {
  id: string
  name: string
  billable: boolean
  /** kept for time already logged, not offered any more */
  archived: boolean
  logCount: number
}

export interface Worklog {
  id: string
  user: Named | null
  minutes: number
  date: string
  note: string | null
  category: Named | null
  billable: boolean
}

export interface IssueLink {
  id: string
  kind: string
  label: string
  outward: boolean
  issue: Issue
}

export interface IssueDetail extends Issue {
  description: any
  watchers: Named[]
  watching: boolean
  links: IssueLink[]
  webLinks: { id: string; url: string; title: string; kind: 'web' | 'wiki'; createdAt: string }[]
  attachments: { id: string; name: string; mime: string; size: number; uploadedBy: string | null; createdAt: string; url: string }[]
  children: Issue[]
  permissions: { edit: boolean; comment: boolean; admin: boolean }
  /** service-level goals (projects with service levels) */
  sla: import('./components/SlaBadges').SlaGoal[]
  /** who it was opened for (no account): by e-mail, on the request form or in the portal;
   *  `mail`: comments for them are e-mailed and they can sign in to the portal */
  requester: { email: string; name: string | null; mail: boolean; source?: 'email' | 'form' | 'portal' } | null
}

/** What it takes to move an issue into a status (crate::workflow). */
export interface StatusRules {
  who?: 'everyone' | 'admins' | 'teams'
  teams?: string[]
  require?: string[]
  comment?: 'off' | 'optional' | 'required'
  resolution?: string
}

export interface Status extends StatusRef {
  position: number
  rules?: StatusRules
}

export interface IssueType extends TypeRef {
  template: any
  /** fields the type hides and requires (custom fields as `custom:<id>`) */
  fields?: { hidden?: string[]; required?: string[] }
}

/** Fields an issue type can hide; all but priority, sprint and security can be required. */
export const TYPE_FIELDS = ['assignee', 'priority', 'labels', 'sprint', 'version', 'affects', 'component', 'storyPoints', 'estimate', 'startDate', 'dueDate', 'security'] as const

export interface Version {
  id: string
  name: string
  description: string | null
  status: 'unreleased' | 'released' | 'archived'
  startDate: string | null
  releaseDate: string | null
  releasedAt: string | null
}

export interface FieldOption {
  id: string
  label: string
  color?: string | null
}

export interface CustomField {
  id: string
  name: string
  kind: 'text' | 'number' | 'select' | 'multiselect' | 'date' | 'user' | 'checkbox' | 'url'
  options: FieldOption[]
  description: string | null
}

export interface Sprint {
  id: string
  name: string
  goal: string | null
  state: 'future' | 'active' | 'closed'
  startAt: string | null
  endAt: string | null
  completedAt?: string | null
  stats?: any
}

export interface ProjectSummary {
  id: string
  key: string
  name: string
  description: string | null
  icon: string | null
  color: string | null
  kind: 'scrum' | 'kanban' | 'tracker'
  leadId: string | null
  leadName?: string | null
  defaultRole: Role
  settings: { estimation?: 'points' | 'hours'; workflow?: 'free' | 'restricted'; wikiSpace?: string; template?: string; defaultAssignee?: string; [k: string]: any }
  createdAt: string
  archivedAt: string | null
  role: Role
  starred: boolean
  openCount?: number
  doneCount?: number
  activeSprint?: string | null
}

export interface Project extends ProjectSummary {
  statuses: Status[]
  types: IssueType[]
  versions: Version[]
  components: { id: string; name: string; description: string | null; leadId: string | null }[]
  fields: CustomField[]
  sprints: Sprint[]
  boards: { id: string; name: string; kind: 'kanban' | 'scrum' }[]
  transitions: { from: string; to: string }[]
  securityLevels: SecurityLevel[]
}

/** Who sees the private issues that carry it — besides project admins and each issue's
 *  reporter, creator and assignee. */
export interface SecurityLevel {
  id: string
  name: string
  description: string
  /** everyone with at least this project role; null: only the teams and people listed */
  minRole: 'viewer' | 'member' | null
  teams: Named[]
  users: Named[]
  /** new issues get it */
  isDefault: boolean
  issueCount: number
  /** the signed-in person belongs to it (and may choose it) */
  mine: boolean
}

export interface BoardColumn {
  id: string
  name: string
  statusIds: string[]
  wipLimit: number | null
}

export interface BoardConfig {
  columns: BoardColumn[]
  swimlanes: 'none' | 'assignee' | 'epic' | 'priority' | 'type'
  quickFilters: { name: string; query: string }[]
  cardFields: string[]
  hideDoneAfterDays: number
  filter?: string
}

export interface BoardData {
  board: { id: string; name: string; kind: 'kanban' | 'scrum'; config: BoardConfig; projectId: string }
  canEdit: boolean
  canConfigure: boolean
  sprint: { id: string; name: string; goal: string | null; startAt: string | null; endAt: string | null } | null
  activeSprints: { id: string; name: string; endAt: string | null }[]
  issues: Issue[]
}

export interface Comment {
  id: string
  author: Named | null
  /** written by someone without an account (an issue's requester: by e-mail or in the portal) */
  external?: { email: string; name: string | null } | null
  body: any
  createdAt: string
  updatedAt: string
  edited: boolean
  /** it came in by e-mail */
  viaEmail?: boolean
  /** written in the service desk portal */
  viaPortal?: boolean
  /** the issue's requester sees it (marked for them, or their own) */
  forRequester?: boolean
  /** when it was e-mailed to the requester */
  emailedAt?: string | null
  /** restricted: only `members`, `admins` or `team:<id>` (and its author) read it */
  visibility?: string | null
  /** the team's name for a team comment */
  visibleTo?: string | null
  reactions: { reaction: string; people: Named[] }[]
}

export interface HistoryEvent {
  id: string
  actor: Named | null
  kind: string
  field: string | null
  old: any
  new: any
  at: string
  key?: string
  title?: string
}

export interface Notification {
  id: string
  actor: Named | null
  kind: 'assigned' | 'mentioned' | 'commented' | 'status' | string
  data: { key: string; title: string; from?: string; to?: string; excerpt?: string; commentId?: string; message?: string; goal?: string; by?: string }
  createdAt: string
  read: boolean
}

export interface SavedView {
  id: string
  ownerId: string | null
  ownerName: string | null
  projectId: string | null
  projectKey: string | null
  name: string
  query: string
  layout: 'list' | 'board' | 'calendar' | 'timeline'
  config: Record<string, any>
  shared: boolean
  starred: boolean
  mine: boolean
}

export interface WikiPage {
  id: string
  title: string
  spaceKey: string
  spaceName: string
  url: string
  updatedAt: string | null
  excerpt: string | null
}

// ---- helpers ----

export function initials(name: string): string {
  const parts = name.trim().split(/\s+/)
  return ((parts[0]?.[0] ?? '') + (parts.length > 1 ? parts[parts.length - 1][0] : '')).toUpperCase() || '?'
}

// all keep white initials at 4.5:1 or better
const AVATAR_COLORS = ['#3b4fd8', '#0b7a55', '#b45309', '#be185d', '#6d28d9', '#0e7490', '#b91c1c', '#4d7c0f', '#9a3412', '#4338ca']

export function userColor(seed: string): string {
  let h = 0
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) >>> 0
  return AVATAR_COLORS[h % AVATAR_COLORS.length]
}

const dateFmt = () => new Intl.DateTimeFormat(locale(), { day: 'numeric', month: 'short' })
const dateYearFmt = () => new Intl.DateTimeFormat(locale(), { day: 'numeric', month: 'short', year: 'numeric' })

/** `12 Nov` (this year) or `12 Nov 2025`. */
export function shortDate(iso: string | null | undefined): string {
  if (!iso) return ''
  const d = new Date(iso.length === 10 ? iso + 'T00:00:00' : iso)
  if (Number.isNaN(d.getTime())) return ''
  return d.getFullYear() === new Date().getFullYear() ? dateFmt().format(d) : dateYearFmt().format(d)
}

export function dateTime(iso: string | null | undefined): string {
  if (!iso) return ''
  return new Intl.DateTimeFormat(locale(), { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(iso))
}

/** "3 minutes ago", "yesterday", … */
export function ago(iso: string | null | undefined): string {
  if (!iso) return ''
  const s = (Date.now() - new Date(iso).getTime()) / 1000
  const rtf = new Intl.RelativeTimeFormat(locale(), { numeric: 'auto' })
  if (Math.abs(s) < 60) return rtf.format(-Math.round(s), 'second')
  if (Math.abs(s) < 3600) return rtf.format(-Math.round(s / 60), 'minute')
  if (Math.abs(s) < 86400) return rtf.format(-Math.round(s / 3600), 'hour')
  if (Math.abs(s) < 86400 * 30) return rtf.format(-Math.round(s / 86400), 'day')
  return shortDate(iso)
}

/** Whole days between now and a date (negative = past). */
export function daysUntil(date: string): number {
  const d = new Date(date + 'T00:00:00')
  const today = new Date()
  today.setHours(0, 0, 0, 0)
  return Math.round((d.getTime() - today.getTime()) / 86400000)
}

/** 90 → "1h 30m" */
export function duration(minutes: number | null | undefined): string {
  if (!minutes) return '0m'
  const h = Math.floor(minutes / 60)
  const m = Math.round(minutes % 60)
  return [h ? `${h}h` : '', m ? `${m}m` : ''].filter(Boolean).join(' ') || '0m'
}

export function isoDate(d: Date): string {
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`
}

/** Quotes a value for TQL when needed. */
export function tqlQuote(s: string): string {
  return /^[\p{L}\p{N}_\-.@+:/#]+$/u.test(s) && !/^(and|or|not|in|is|empty|null|order|by)$/i.test(s) ? s : `"${s.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`
}

/** Project actions whose minimum role each project chooses (crate::access::ACTIONS). */
export const PROJECT_ACTIONS = {
  deleteIssues: { default: 'member', nobody: false },
  moveIssues: { default: 'member', nobody: false },
  manageSprints: { default: 'member', nobody: false },
  manageReleases: { default: 'member', nobody: false },
  moderateComments: { default: 'admin', nobody: true },
  logTimeForOthers: { default: 'admin', nobody: true },
  setSecurity: { default: 'member', nobody: false },
} as const
export type ProjectAction = keyof typeof PROJECT_ACTIONS

/** Whether the signed-in person may do `action` in `project` (the server decides in the end). */
export function may(project: Project, action: ProjectAction): boolean {
  const set = (project.settings?.permissions as Record<string, string> | undefined)?.[action] ?? PROJECT_ACTIONS[action].default
  if (set === 'nobody') return false
  const rank = { none: 0, viewer: 1, member: 2, admin: 3 }
  return rank[project.role] >= rank[set === 'admin' ? 'admin' : 'member']
}
