import { t, AVAILABLE, LANGUAGES, locale } from '../i18n'
import { useState, type FormEvent, type ReactNode } from 'react'
import { useLocation, useNavigate } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { BackupImportTab } from './BackupImport'
import {
  Archive, ArchiveRestore, Check, ChevronDown, CircleDot, Clock, Ellipsis, FolderKanban, HardDrive, LayoutDashboard, Link, Mail, MessageSquare, Paperclip, Pencil, Plus, ScrollText, Search, Send, Shield, ShieldOff, Trash,
  UserCheck, UserPlus, UserX, Users, Webhook,
} from 'lucide-react'
import { api, ago, dateTime, shortDate, type Me, type TimeCategory } from '../api'
import { Avatar, Confirm, Empty, ErrorBox, Loading, Modal, PopButton, Tabs, useMe, usePeople, useProjects, useSetup, useToast } from '../ui'
import { PROJECT_COLORS } from '../icons'
import { CopyField, Switch } from './Settings'
import { DeleteTeam, TeamDialog, TeamMembers, useTeams, type Team } from './People'
import { SignInBadges, useSignInOptions, type SignInMethods } from '../components/Sso'
import { ResetTwoFactor, TwoFactorPolicy, canResetTwoFactor, useTwoFactorPolicy } from '../components/Security'
import './Admin.css'

type Tab = 'overview' | 'users' | 'teams' | 'time' | 'webhooks' | 'import' | 'audit'
const TABS: Tab[] = ['overview', 'users', 'teams', 'time', 'webhooks', 'import', 'audit']

export default function Admin() {
  const me = useMe().data
  const loc = useLocation()
  const nav = useNavigate()
  const seg = loc.pathname.replace(/^\/admin\/?/, '').split('/')[0] as Tab
  const tab: Tab = TABS.includes(seg) ? seg : 'overview'
  const role = me?.user?.role
  if (!me?.user) return <Loading />
  if (role !== 'owner' && role !== 'admin') {
    return (
      <div className="page">
        <Empty icon={<Shield size={34} />} title={t('Administration is for workspace owners and admins')}>
          {t('Ask an admin of {workspace} if something needs to change here.', { workspace: me.workspace?.name ?? 'TaskCraft' })}
        </Empty>
      </div>
    )
  }
  return (
    <div className="page adm-page">
      <div className="page-head">
        <h1><Shield size={20} /> {t('Administration')}</h1>
        <span className="faint">{me.workspace?.name}</span>
      </div>
      <Tabs
        value={tab}
        onChange={(v) => nav(v === 'overview' ? '/admin' : `/admin/${v}`)}
        tabs={[
          { id: 'overview', label: <><LayoutDashboard size={14} /> {t('Overview')}</> },
          { id: 'users', label: <><Users size={14} /> {t('Users')}</> },
          { id: 'teams', label: <><CircleDot size={14} /> {t('Teams')}</> },
          { id: 'time', label: <><Clock size={14} /> {t('Time tracking')}</> },
          { id: 'webhooks', label: <><Webhook size={14} /> {t('Webhooks')}</> },
          { id: 'import', label: <><ArchiveRestore size={14} /> {t('Import')}</> },
          { id: 'audit', label: <><ScrollText size={14} /> {t('Audit log')}</> },
        ]}
      />
      {tab === 'overview' && <Overview me={me} />}
      {tab === 'users' && <UsersTab me={me} />}
      {tab === 'teams' && <TeamsTab />}
      {tab === 'time' && <TimeTab />}
      {tab === 'webhooks' && <WebhooksTab />}
      {tab === 'import' && <BackupImportTab />}
      {tab === 'audit' && <AuditTab />}
    </div>
  )
}

// ---- overview ----

interface Stats {
  users: number
  projects: number
  issues: number
  comments: number
  attachments: number
  attachmentBytes: number
}

function formatBytes(n: number): string {
  const units = ['byte', 'kilobyte', 'megabyte', 'gigabyte', 'terabyte']
  let v = n
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  // the short form of a plain byte count reads "0 byte" in English; spell it out instead
  return new Intl.NumberFormat(locale(), { style: 'unit', unit: units[i], unitDisplay: i === 0 ? 'long' : 'short', maximumFractionDigits: i > 0 && v < 10 ? 1 : 0 }).format(v)
}

function Overview({ me }: { me: Me }) {
  const stats = useQuery({ queryKey: ['admin', 'stats'], queryFn: () => api.get<Stats>('/admin/stats') })
  const s = stats.data
  const nf = new Intl.NumberFormat(locale())
  const num = (v: number | undefined) => (v === undefined ? undefined : nf.format(v))
  return (
    <>
      {stats.error && <ErrorBox error={stats.error} />}
      <div className="adm-stats">
        <Stat icon={<Users size={16} />} value={num(s?.users)} label={t('Active people')} />
        <Stat icon={<FolderKanban size={16} />} value={num(s?.projects)} label={t('Projects')} />
        <Stat icon={<CircleDot size={16} />} value={num(s?.issues)} label={t('Issues')} />
        <Stat icon={<MessageSquare size={16} />} value={num(s?.comments)} label={t('Comments')} />
        <Stat icon={<Paperclip size={16} />} value={num(s?.attachments)} label={t('Attachments')} />
        <Stat icon={<HardDrive size={16} />} value={s ? formatBytes(s.attachmentBytes) : undefined} label={t('Attachment storage')} />
      </div>
      <WorkspaceForm key={JSON.stringify(me.workspace)} me={me} />
      <MailCard me={me} />
      <TwoFactorPolicy />
    </>
  )
}

interface MailStatus {
  enabled: boolean
  from: string
  server: string | null
  pending: number
  failed: number
  sentLastDay: number
  lastError: string | null
  lastErrorAt: string | null
}

function MailCard({ me }: { me: Me }) {
  const qc = useQueryClient()
  const toast = useToast()
  const mail = useQuery({ queryKey: ['admin', 'mail'], queryFn: () => api.get<MailStatus>('/admin/mail') })
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const m = mail.data
  const nf = new Intl.NumberFormat(locale())
  const sendTest = async () => {
    setBusy(true)
    setErr('')
    try {
      const r = await api.post<{ to: string }>('/admin/mail/test')
      toast(t('Test e-mail sent to {email}', { email: r.to }))
      qc.invalidateQueries({ queryKey: ['admin', 'mail'] })
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <section className="adm-card adm-mail" aria-labelledby="adm-mail-title">
      <div className="adm-card-head">
        <h2 id="adm-mail-title"><Mail size={15} /> {t('E-mail')}</h2>
        <p className="small muted">{t('Invitations, password-reset links and notification e-mails.')}</p>
      </div>
      <div className="adm-card-body">
        {mail.isLoading && <Loading />}
        {mail.error && <ErrorBox error={mail.error} />}
        {m && (m.enabled ? (
          <>
            <span className="badge badge-success"><Check size={12} /> {t('Configured')}</span>
            <dl className="adm-mail-facts">
              <dt>{t('Sender')}</dt>
              <dd>{m.from}</dd>
              <dt>{t('Mail server')}</dt>
              <dd>{m.server}</dd>
              <dt>{t('Sent in the last 24 hours')}</dt>
              <dd>{nf.format(m.sentLastDay)}</dd>
              <dt>{t('Waiting to be sent')}</dt>
              <dd>{nf.format(m.pending)}</dd>
              {m.failed > 0 && (
                <>
                  <dt>{t('Gave up (last 7 days)')}</dt>
                  <dd>{nf.format(m.failed)}</dd>
                </>
              )}
            </dl>
            {m.lastError && (
              <p className="error-text small adm-mail-error">
                {t('Last error: {error}', { error: m.lastError })}{m.lastErrorAt && <span className="faint"> · {ago(m.lastErrorAt)}</span>}
              </p>
            )}
          </>
        ) : (
          <>
            <span className="badge badge-warning">{t('Not configured')}</span>
            <p className="small muted">
              {t('Set TASKCRAFT_SMTP_URL (and TASKCRAFT_MAIL_FROM for the sender) to e-mail invitations, password-reset links and notifications. Until then, you pass invitation and reset links on yourself.')}
            </p>
          </>
        ))}
      </div>
      <div className="adm-card-foot">
        {err && <span className="error-text adm-foot-msg" role="alert">{err}</span>}
        {m?.enabled && me.user?.email && <span className="faint small">{t('Goes to {email}', { email: me.user.email })}</span>}
        <button type="button" className="btn" disabled={busy || !m?.enabled} onClick={sendTest}><Send size={14} /> {busy ? t('Sending…') : t('Send test e-mail')}</button>
      </div>
    </section>
  )
}

function Stat({ icon, value, label }: { icon: ReactNode; value: string | undefined; label: string }) {
  return (
    <div className="adm-stat">
      <span className="adm-stat-icon">{icon}</span>
      <div>
        <div className="adm-stat-value">{value ?? '–'}</div>
        <div className="adm-stat-label">{label}</div>
      </div>
    </div>
  )
}

const HEX = /^#[0-9a-fA-F]{6}$/

function WorkspaceForm({ me }: { me: Me }) {
  const ws = me.workspace!
  const qc = useQueryClient()
  const toast = useToast()
  const [name, setName] = useState(ws.name)
  const [accent, setAccent] = useState(ws.accentColor ?? '')
  const [lang, setLang] = useState(ws.defaultLanguage ?? '')
  const [weekStart, setWeekStart] = useState<'monday' | 'sunday'>(ws.weekStart === 'sunday' ? 'sunday' : 'monday')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const accentOk = !accent || HEX.test(accent)
  const dirty = name.trim() !== ws.name || accent !== (ws.accentColor ?? '') || lang !== (ws.defaultLanguage ?? '') || weekStart !== (ws.weekStart === 'sunday' ? 'sunday' : 'monday')
  const save = async (e: FormEvent) => {
    e.preventDefault()
    if (!name.trim()) return setErr(t('The workspace needs a name.'))
    if (!accentOk) return setErr(t('The accent colour looks like #4f6bed.'))
    setBusy(true)
    setErr('')
    try {
      await api.put('/admin/workspace', { name: name.trim(), accentColor: accent ? accent.toLowerCase() : null, defaultLanguage: lang || null, weekStart })
      await qc.invalidateQueries({ queryKey: ['me'] })
      toast(t('Workspace settings saved'))
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <form className="adm-card" onSubmit={save}>
      <div className="adm-card-head">
        <h2>{t('Workspace')}</h2>
        <p className="small muted">{t('Name, colour and defaults for everyone in the workspace.')}</p>
      </div>
      <div className="adm-card-body adm-ws-grid">
        <div className="field">
          <label htmlFor="adm-ws-name">{t('Workspace name')}</label>
          <input id="adm-ws-name" className="input" value={name} onChange={(e) => setName(e.target.value)} maxLength={100} />
        </div>
        <div className="field">
          <label htmlFor="adm-ws-accent">{t('Accent colour')}</label>
          <div className="adm-accent">
            <input type="color" className="adm-color" value={HEX.test(accent) ? accent : PROJECT_COLORS[0]} onChange={(e) => setAccent(e.target.value)} aria-label={t('Pick a colour')} />
            <input id="adm-ws-accent" className={`input adm-hex ${accentOk ? '' : 'adm-invalid'}`} value={accent} onChange={(e) => setAccent(e.target.value.trim())} placeholder={t('Default')} maxLength={7} spellCheck={false} aria-invalid={!accentOk} />
            {accent && <button type="button" className="btn btn-sm btn-ghost" onClick={() => setAccent('')}>{t('Use default')}</button>}
          </div>
          <div className="swatches adm-swatches">
            {PROJECT_COLORS.map((c) => (
              <button key={c} type="button" className={`swatch ${c === accent.toLowerCase() ? 'on' : ''}`} style={{ background: c }} aria-label={c} aria-pressed={c === accent.toLowerCase()} onClick={() => setAccent(c)} />
            ))}
          </div>
          <span className="field-hint">{t('Leave empty for the built-in colour.')}</span>
        </div>
        <div className="field">
          <label htmlFor="adm-ws-lang">{t('Default language')}</label>
          <select id="adm-ws-lang" className="select" value={lang} onChange={(e) => setLang(e.target.value)}>
            <option value="">{t('Each person’s browser language')}</option>
            {AVAILABLE.map((l) => <option key={l.code} value={l.code} lang={l.code}>{l.name}</option>)}
            {lang && !AVAILABLE.some((l) => l.code === lang) && <option value={lang}>{LANGUAGES.find((l) => l.code === lang)?.name ?? lang}</option>}
          </select>
          <span className="field-hint">{t('People can still pick their own language in their profile.')}</span>
        </div>
        <div className="field">
          <span className="field-label" id="adm-ws-week">{t('Weeks start on')}</span>
          <div className="seg adm-seg" role="radiogroup" aria-labelledby="adm-ws-week">
            <button type="button" role="radio" aria-checked={weekStart === 'monday'} className={weekStart === 'monday' ? 'active' : ''} onClick={() => setWeekStart('monday')}>{t('Monday')}</button>
            <button type="button" role="radio" aria-checked={weekStart === 'sunday'} className={weekStart === 'sunday' ? 'active' : ''} onClick={() => setWeekStart('sunday')}>{t('Sunday')}</button>
          </div>
          <span className="field-hint">{t('Used by calendars and timesheets.')}</span>
        </div>
      </div>
      <div className="adm-card-foot">
        {err && <span className="error-text adm-foot-msg" role="alert">{err}</span>}
        <button type="submit" className="btn btn-primary" disabled={busy || !dirty}>{t('Save changes')}</button>
      </div>
    </form>
  )
}

// ---- users ----

interface AdminUser {
  id: string
  name: string
  email: string | null
  role: 'owner' | 'admin' | 'member' | 'guest'
  status: 'active' | 'invited' | 'deactivated'
  title: string | null
  createdAt: string
  lastLoginAt: string | null
  /** password and / or single sign-on providers */
  signIn?: SignInMethods | null
  /** removed for good (anonymized; the history stays) */
  removed?: boolean
}

const ROLES: { id: 'admin' | 'member' | 'guest'; label: () => string; desc: () => string }[] = [
  { id: 'admin', label: () => t('Admin'), desc: () => t('Admin of every project; manages people, teams and workspace settings.') },
  { id: 'member', label: () => t('Member'), desc: () => t('Works in projects open to the workspace and the ones they’re added to.') },
  { id: 'guest', label: () => t('Guest'), desc: () => t('Sees only projects they’re added to, and no e-mail addresses.') },
]

const roleName = (r: string) => (r === 'owner' ? t('Owner') : ROLES.find((x) => x.id === r)?.label() ?? r)

function StatusBadge({ status, removed }: { status: AdminUser['status']; removed?: boolean }) {
  if (removed) return <span className="badge">{t('Removed')}</span>
  if (status === 'active') return <span className="badge badge-success">{t('Active')}</span>
  if (status === 'invited') return <span className="badge badge-accent">{t('Invited')}</span>
  return <span className="badge">{t('Deactivated')}</span>
}

function UsersTab({ me }: { me: Me }) {
  const qc = useQueryClient()
  // with sign-in through single sign-on only, passwords (and links to reset them) don't work
  const passwords = useSignInOptions().data?.passwordLogin !== false
  const toast = useToast()
  const users = useQuery({ queryKey: ['admin', 'users'], queryFn: () => api.get<AdminUser[]>('/users') })
  const [q, setQ] = useState('')
  const [status, setStatus] = useState<'all' | AdminUser['status']>('all')
  const [adding, setAdding] = useState(false)
  const [link, setLink] = useState<{ user: AdminUser; url: string; emailed: boolean } | null>(null)
  const [deactivating, setDeactivating] = useState<AdminUser | null>(null)
  const [removing, setRemoving] = useState<AdminUser | null>(null)
  // two-factor authentication: who has it, resetting it
  const enrolled = useTwoFactorPolicy().data?.enrolled
  const [resetting, setResetting] = useState<AdminUser | null>(null)
  const myId = me.user!.id
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['admin', 'users'] })
    qc.invalidateQueries({ queryKey: ['admin', 'stats'] })
    qc.invalidateQueries({ queryKey: ['people'] })
  }
  const patch = async (u: AdminUser, body: Record<string, string>, done: string) => {
    try {
      await api.patch(`/users/${u.id}`, body)
      refresh()
      toast(done)
    } catch (e) {
      toast((e as Error).message)
    }
  }
  const makeLink = async (u: AdminUser) => {
    try {
      const r = await api.post<{ url: string; emailed?: boolean }>(`/users/${u.id}/link`)
      setLink({ user: u, url: r.url, emailed: !!r.emailed })
    } catch (e) {
      toast((e as Error).message)
    }
  }
  const all = users.data ?? []
  const counts = { all: all.length, active: 0, invited: 0, deactivated: 0 }
  for (const u of all) counts[u.status]++
  const needle = q.trim().toLowerCase()
  const list = all.filter((u) => (status === 'all' || u.status === status) && (!needle || `${u.name} ${u.email ?? ''} ${u.title ?? ''}`.toLowerCase().includes(needle)))
  const STATUSES: { id: 'all' | AdminUser['status']; label: string }[] = [
    { id: 'all', label: t('All') },
    { id: 'active', label: t('Active') },
    { id: 'invited', label: t('Invited') },
    { id: 'deactivated', label: t('Deactivated') },
  ]

  return (
    <>
      <div className="adm-toolbar">
        <div className="adm-search">
          <Search size={15} aria-hidden />
          <input className="input" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t('Find people…')} aria-label={t('Find people')} />
        </div>
        <div className="adm-chips" role="group" aria-label={t('Filter by status')}>
          {STATUSES.map((s) => (
            <button key={s.id} type="button" className={`chip ${status === s.id ? 'on' : ''}`} aria-pressed={status === s.id} onClick={() => setStatus(s.id)}>
              {s.label} <span className="adm-chip-count">{counts[s.id]}</span>
            </button>
          ))}
        </div>
        <span className="spacer" />
        <button className="btn btn-primary" onClick={() => setAdding(true)}><UserPlus size={15} /> {t('Add person')}</button>
      </div>
      {users.isLoading && <Loading />}
      {users.error && <ErrorBox error={users.error} />}
      {users.data && list.length === 0 && <Empty icon={<Users size={32} />} title={t('Nobody matches')} />}
      {list.length > 0 && (
        <div className="adm-table-wrap">
          <table className="list adm-table adm-users">
            <thead>
              <tr>
                <th>{t('Person')}</th>
                <th>{t('Role')}</th>
                <th>{t('Status')}</th>
                <th className="adm-hide-sm">{t('Signs in with')}</th>
                <th className="adm-hide-sm">{t('Last signed in')}</th>
                <th><span className="sr-only">{t('Actions')}</span></th>
              </tr>
            </thead>
            <tbody>
              {list.map((u) => {
                const self = u.id === myId
                const locked = u.role === 'owner' || !!u.removed
                return (
                  <tr key={u.id} className={u.status === 'deactivated' ? 'adm-off' : ''}>
                    <td>
                      <div className="adm-person">
                        <Avatar name={u.name} id={u.id} size={30} />
                        <div className="adm-person-text">
                          <div className="adm-person-name">{u.name} {self && <span className="badge">{t('You')}</span>} {enrolled?.includes(u.id) && <span className="badge badge-success" title={t('Two-factor authentication is on')}>{t('2FA')}</span>}</div>
                          <div className="faint small adm-ellipsis">{[u.email, u.title].filter(Boolean).join(' · ')}</div>
                        </div>
                      </div>
                    </td>
                    <td>
                      {locked || self ? (
                        <span className="adm-role-fixed" title={u.removed ? t('This person was removed') : locked ? t('The owner can’t be changed') : t('You can’t change your own role')}>{roleName(u.role)}</span>
                      ) : (
                        <PopButton className="btn btn-sm adm-role-btn" label={<>{roleName(u.role)} <ChevronDown size={13} /></>} width={320} title={t('Change role of {name}', { name: u.name })}>
                          {(close) => (
                            <div className="choices-list" role="listbox">
                              {ROLES.map((r) => (
                                <button
                                  key={r.id}
                                  type="button"
                                  role="option"
                                  aria-selected={u.role === r.id}
                                  className={`menu-item ${u.role === r.id ? 'selected' : ''}`}
                                  onClick={() => {
                                    close()
                                    if (r.id !== u.role) patch(u, { role: r.id }, t('{name} is now {role}', { name: u.name, role: r.label() }))
                                  }}
                                >
                                  <span>
                                    <div>{r.label()}</div>
                                    <div className="mi-desc">{r.desc()}</div>
                                  </span>
                                </button>
                              ))}
                            </div>
                          )}
                        </PopButton>
                      )}
                    </td>
                    <td><StatusBadge status={u.status} removed={u.removed} /></td>
                    <td className="small adm-hide-sm"><SignInBadges methods={u.signIn} /></td>
                    <td className="small adm-hide-sm" title={u.lastLoginAt ? dateTime(u.lastLoginAt) : undefined}>
                      {u.lastLoginAt ? ago(u.lastLoginAt) : <span className="faint">{u.status === 'invited' ? t('Not yet') : t('Never')}</span>}
                    </td>
                    <td className="adm-actions">
                      {!u.removed && <PopButton className="icon-btn" label={<Ellipsis size={16} />} title={t('Actions for {name}', { name: u.name })} align="end" width={240}>
                        {(close) => (
                          <div className="choices-list">
                            {(passwords || u.status === 'invited') && (
                              <button type="button" className="menu-item" onClick={() => { close(); makeLink(u) }}>
                                <Link size={15} /> {u.status === 'invited' ? t('New invitation link') : t('Password-reset link')}
                              </button>
                            )}
                            {canResetTwoFactor(me, u, enrolled) && (
                              <button type="button" className="menu-item" onClick={() => { close(); setResetting(u) }}>
                                <ShieldOff size={15} /> {t('Reset two-factor authentication')}
                              </button>
                            )}
                            {!locked && !self && u.status !== 'deactivated' && (
                              <button type="button" className="menu-item danger" onClick={() => { close(); setDeactivating(u) }}>
                                <UserX size={15} /> {t('Deactivate')}
                              </button>
                            )}
                            {!locked && !self && u.status === 'deactivated' && (
                              <button type="button" className="menu-item" onClick={() => { close(); patch(u, { status: 'active' }, t('{name} was reactivated', { name: u.name })) }}>
                                <UserCheck size={15} /> {t('Reactivate')}
                              </button>
                            )}
                            {!self && u.role !== 'owner' && (u.role !== 'admin' || me.user?.role === 'owner') && (
                              <button type="button" className="menu-item danger" onClick={() => { close(); setRemoving(u) }}>
                                <Trash size={15} /> {t('Remove…')}
                              </button>
                            )}
                            <div className="menu-empty adm-menu-note">{t('Joined {date}', { date: shortDate(u.createdAt) })}</div>
                          </div>
                        )}
                      </PopButton>}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
        </div>
      )}
      {adding && <AddPerson onClose={() => setAdding(false)} onCreated={refresh} />}
      {link && (
        <Modal title={link.user.status === 'invited' ? t('Invitation link') : t('Password-reset link')} onClose={() => setLink(null)} width={540} footer={<button className="btn btn-primary" onClick={() => setLink(null)}>{t('Done [[close]]')}</button>}>
          <LinkResult name={link.user.name} url={link.url} invite={link.user.status === 'invited'} emailedTo={link.emailed ? link.user.email : null} />
        </Modal>
      )}
      {deactivating && (
        <Confirm
          title={t('Deactivate {name}?', { name: deactivating.name })}
          body={<p>{t('{name} is signed out everywhere and their API tokens are deleted. Their issues and comments stay, and you can reactivate them later.', { name: deactivating.name })}</p>}
          confirmLabel={t('Deactivate')}
          danger
          onClose={() => setDeactivating(null)}
          onConfirm={() => patch(deactivating, { status: 'deactivated' }, t('{name} was deactivated', { name: deactivating.name }))}
        />
      )}
      {removing && (
        <Confirm
          title={t('Remove {name}?', { name: removing.name })}
          body={
            <>
              <p>{t('Their comments, time logs and history stay, shown as “Former user”. Everything personal is deleted: name, e-mail address, password, sign-in methods, notifications, private filters and dashboards. Their open issues become unassigned.')}</p>
              <p><strong>{t('This can’t be undone.')}</strong> {t('To keep the account for later, deactivate it instead.')}</p>
            </>
          }
          confirmLabel={t('Remove')}
          danger
          onClose={() => setRemoving(null)}
          onConfirm={async () => {
            try {
              await api.del(`/users/${removing.id}`)
              refresh()
              toast(t('{name} was removed', { name: removing.name }))
            } catch (e) {
              toast((e as Error).message)
            }
          }}
        />
      )}
      {resetting && <ResetTwoFactor user={resetting} onClose={() => setResetting(null)} />}
    </>
  )
}

function LinkResult({ name, url, invite, emailedTo }: { name: string; url: string; invite: boolean; emailedTo?: string | null }) {
  return (
    <div className="adm-link">
      <p>
        {emailedTo
          ? invite
            ? t('We e-mailed this invitation to {email}. You can also send {name} the link yourself: they choose a password and are signed in.', { name, email: emailedTo })
            : t('We e-mailed this link to {email}. You can also pass it on yourself: it lets {name} choose a new password and signs them out elsewhere.', { name, email: emailedTo })
          : invite
            ? t('TaskCraft doesn’t send e-mail. Send this link to {name} yourself: they choose a password and are signed in.', { name })
            : t('TaskCraft doesn’t send e-mail. Send this link to {name} yourself: it lets them choose a new password and signs them out elsewhere.', { name })}
      </p>
      <CopyField value={url} label={invite ? t('Invitation link') : t('Password-reset link')} />
      <p className="faint small">{t('The link works once and expires in 7 days. Creating a new one replaces it.')}</p>
    </div>
  )
}

function AddPerson({ onClose, onCreated }: { onClose: () => void; onCreated: () => void }) {
  const toast = useToast()
  const passwords = useSignInOptions().data?.passwordLogin !== false
  const [name, setName] = useState('')
  const [email, setEmail] = useState('')
  const [role, setRole] = useState<'admin' | 'member' | 'guest'>('member')
  const [mode, setMode] = useState<'invite' | 'password'>('invite')
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const [result, setResult] = useState<{ name: string; url: string; emailedTo: string | null } | null>(null)
  const mail = !!useSetup().data?.mail
  const submit = async (e?: FormEvent) => {
    e?.preventDefault()
    if (!name.trim() || !email.trim()) return setErr(t('Name and e-mail address are required.'))
    if (mode === 'password' && password.length < 8) return setErr(t('The password must be at least 8 characters.'))
    setBusy(true)
    setErr('')
    try {
      const r = await api.post<{ id: string; inviteUrl?: string | null; emailed?: boolean }>('/users', { name: name.trim(), email: email.trim(), role, password: mode === 'password' ? password : undefined })
      onCreated()
      if (r.inviteUrl) setResult({ name: name.trim(), url: r.inviteUrl, emailedTo: r.emailed ? email.trim() : null })
      else {
        toast(t('{name} can sign in now', { name: name.trim() }))
        onClose()
      }
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  if (result) {
    return (
      <Modal title={t('{name} is invited', { name: result.name })} onClose={onClose} width={540} footer={<button className="btn btn-primary" onClick={onClose}>{t('Done [[close]]')}</button>}>
        <LinkResult name={result.name} url={result.url} invite emailedTo={result.emailedTo} />
      </Modal>
    )
  }
  return (
    <Modal
      title={t('Add person')}
      onClose={onClose}
      width={560}
      footer={
        <>
          {err && <span className="error-text adm-foot-msg" role="alert">{err}</span>}
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !name.trim() || !email.trim()} onClick={() => submit()}>{mode === 'invite' ? t('Create invitation') : t('Create account')}</button>
        </>
      }
    >
      <form onSubmit={submit}>
        <div className="adm-grid2">
          <div className="field">
            <label htmlFor="adm-new-name">{t('Name')}</label>
            <input id="adm-new-name" className="input" autoFocus value={name} onChange={(e) => setName(e.target.value)} autoComplete="off" />
          </div>
          <div className="field">
            <label htmlFor="adm-new-email">{t('E-mail address')}</label>
            <input id="adm-new-email" type="email" className="input" value={email} onChange={(e) => setEmail(e.target.value)} autoComplete="off" />
          </div>
        </div>
        <div className="field">
          <span className="field-label" id="adm-new-role">{t('Role')}</span>
          <div className="adm-roles" role="radiogroup" aria-labelledby="adm-new-role">
            {ROLES.map((r) => (
              <button key={r.id} type="button" role="radio" aria-checked={role === r.id} className={`template ${role === r.id ? 'on' : ''}`} onClick={() => setRole(r.id)}>
                <strong>{r.label()}</strong>
                <span className="faint small">{r.desc()}</span>
              </button>
            ))}
          </div>
        </div>
        <div className="field">
          <span className="field-label" id="adm-new-mode">{t('Signing in')}</span>
          <div className="seg adm-seg" role="radiogroup" aria-labelledby="adm-new-mode">
            <button type="button" role="radio" aria-checked={mode === 'invite'} className={mode === 'invite' ? 'active' : ''} onClick={() => setMode('invite')}>{t('Invitation link')}</button>
            {passwords && <button type="button" role="radio" aria-checked={mode === 'password'} className={mode === 'password' ? 'active' : ''} onClick={() => setMode('password')}>{t('Set a password now')}</button>}
          </div>
          <span className="field-hint">
            {!passwords
              ? t('They sign in through single sign-on; the invitation link takes them there.')
              : mode === 'invite'
              ? mail
                ? t('They get an e-mail with a link to choose their own password; you see the link too.')
                : t('You get a link to send them; they choose their own password. No e-mail is sent.')
              : t('Tell them the password; they can change it in their profile.')}
          </span>
        </div>
        {mode === 'password' && (
          <div className="field">
            <label htmlFor="adm-new-pw">{t('Password')}</label>
            <input id="adm-new-pw" type="password" className="input" value={password} onChange={(e) => setPassword(e.target.value)} autoComplete="new-password" minLength={8} />
            <span className="field-hint">{t('At least 8 characters.')}</span>
          </div>
        )}
        <button type="submit" hidden />
      </form>
    </Modal>
  )
}

// ---- teams ----

function TeamsTab() {
  const teams = useTeams()
  const people = usePeople()
  const [editing, setEditing] = useState<Team | 'new' | null>(null)
  const [deleting, setDeleting] = useState<Team | null>(null)
  return (
    <>
      <div className="adm-toolbar">
        <p className="small muted adm-intro">{t('Teams group people; projects can give a whole team access at once. Members can create and edit teams, only admins delete them.')}</p>
        <span className="spacer" />
        <button className="btn btn-primary" onClick={() => setEditing('new')}><Plus size={15} /> {t('New team')}</button>
      </div>
      {teams.isLoading && <Loading />}
      {teams.error && <ErrorBox error={teams.error} />}
      {teams.data && teams.data.length === 0 && <Empty icon={<Users size={32} />} title={t('No teams yet')} />}
      {(teams.data?.length ?? 0) > 0 && (
        <div className="adm-table-wrap">
          <table className="list adm-table">
            <thead>
              <tr>
                <th>{t('Team')}</th>
                <th>{t('Members')}</th>
                <th><span className="sr-only">{t('Actions')}</span></th>
              </tr>
            </thead>
            <tbody>
              {teams.data!.map((tm) => (
                <tr key={tm.id}>
                  <td>
                    <div className="adm-person">
                      <span className="adm-team-swatch" style={{ background: tm.color ?? 'var(--text-faint)' }} aria-hidden />
                      <div className="adm-person-text">
                        <div className="adm-person-name">{tm.name}</div>
                        {tm.description && <div className="faint small adm-ellipsis">{tm.description}</div>}
                      </div>
                    </div>
                  </td>
                  <td>
                    <div className="adm-team-members">
                      <TeamMembers team={tm} people={people.data ?? []} max={6} />
                      <span className="faint small">{t('{count} members', { count: tm.memberIds.length })}</span>
                    </div>
                  </td>
                  <td className="adm-actions">
                    <button className="icon-btn" title={t('Edit team')} aria-label={t('Edit team')} onClick={() => setEditing(tm)}><Pencil size={15} /></button>
                    <button className="icon-btn" title={t('Delete team')} aria-label={t('Delete team')} onClick={() => setDeleting(tm)}><Trash size={15} /></button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {editing && <TeamDialog team={editing === 'new' ? undefined : editing} onClose={() => setEditing(null)} />}
      {deleting && <DeleteTeam team={deleting} onClose={() => setDeleting(null)} />}
    </>
  )
}

// ---- time tracking ----

function TimeTab() {
  const qc = useQueryClient()
  const toast = useToast()
  const list = useQuery({ queryKey: ['time-categories'], queryFn: () => api.get<TimeCategory[]>('/time-categories') })
  const [name, setName] = useState('')
  const [billable, setBillable] = useState(false)
  const [deleting, setDeleting] = useState<TimeCategory | null>(null)
  const run = async (fn: () => Promise<unknown>) => {
    try {
      await fn()
    } catch (e) {
      toast((e as Error).message)
    } finally {
      qc.invalidateQueries({ queryKey: ['time-categories'] })
    }
  }
  const add = async (e: FormEvent) => {
    e.preventDefault()
    if (!name.trim()) return
    await run(() => api.post('/time-categories', { name: name.trim(), billable }))
    setName('')
    setBillable(false)
  }
  return (
    <>
      <div className="adm-toolbar">
        <p className="small muted adm-intro">{t('Time categories say what time is spent on — like Development, Meetings or Support — and whether such time is billable unless the person logging it says otherwise. They show up when logging time and in the time report.')}</p>
      </div>
      {list.isLoading && <Loading />}
      {list.error && <ErrorBox error={list.error} />}
      {list.data && list.data.length === 0 && <Empty icon={<Clock size={32} />} title={t('No time categories yet')} />}
      {(list.data?.length ?? 0) > 0 && (
        <div className="adm-table-wrap">
          <table className="list adm-table">
            <thead>
              <tr>
                <th>{t('Category')}</th>
                <th>{t('Billable by default')}</th>
                <th>{t('Time logs')}</th>
                <th><span className="sr-only">{t('Actions')}</span></th>
              </tr>
            </thead>
            <tbody>
              {list.data!.map((c) => (
                <tr key={c.id} className={c.archived ? 'adm-archived' : undefined}>
                  <td>
                    <input
                      className="input input-sm adm-cat-name"
                      defaultValue={c.name}
                      aria-label={t('Name of {name}', { name: c.name })}
                      onBlur={(e) => e.target.value.trim() && e.target.value.trim() !== c.name && run(() => api.patch(`/time-categories/${c.id}`, { name: e.target.value.trim() }))}
                      onKeyDown={(e) => e.key === 'Enter' && (e.target as HTMLInputElement).blur()}
                    />
                    {c.archived && <span className="badge" style={{ marginInlineStart: 8 }}>{t('Archived')}</span>}
                  </td>
                  <td><Switch checked={c.billable} label={t('{name} is billable by default', { name: c.name })} onChange={(on) => run(() => api.patch(`/time-categories/${c.id}`, { billable: on }))} /></td>
                  <td className="faint small">{c.logCount}</td>
                  <td className="adm-actions">
                    <button
                      className="icon-btn"
                      title={c.archived ? t('Offer it again') : t('Archive: keep it for time already logged, don’t offer it any more')}
                      aria-label={c.archived ? t('Restore {name}', { name: c.name }) : t('Archive {name}', { name: c.name })}
                      onClick={() => run(() => api.patch(`/time-categories/${c.id}`, { archived: !c.archived }))}
                    >
                      {c.archived ? <ArchiveRestore size={15} /> : <Archive size={15} />}
                    </button>
                    <button className="icon-btn" title={t('Delete')} aria-label={t('Delete {name}', { name: c.name })} onClick={() => setDeleting(c)}><Trash size={15} /></button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <form className="row-wrap adm-cat-add" onSubmit={add}>
        <input className="input input-sm" placeholder={t('New category, like Development')} aria-label={t('New category name')} value={name} onChange={(e) => setName(e.target.value)} />
        <label className="row small" style={{ gap: 6 }}>
          <input type="checkbox" checked={billable} onChange={(e) => setBillable(e.target.checked)} /> {t('Billable by default')}
        </label>
        <button className="btn btn-sm btn-primary" type="submit" disabled={!name.trim()}><Plus size={14} /> {t('Add category')}</button>
      </form>
      {deleting && (
        <Confirm
          title={t('Delete time category {name}?', { name: deleting.name })}
          body={<p>{deleting.logCount ? t('{count} time logs lose the category (they stay billable or not as they are). Archiving keeps it for them instead.', { count: deleting.logCount }) : t('No time is logged with it.')}</p>}
          confirmLabel={t('Delete category')}
          danger
          onConfirm={() => run(() => api.del(`/time-categories/${deleting.id}`))}
          onClose={() => setDeleting(null)}
        />
      )}
    </>
  )
}

// ---- webhooks ----

interface Hook {
  id: string
  projectId: string | null
  projectKey: string | null
  url: string
  events: string[]
  active: boolean
  lastStatus: number | null
  lastError: string | null
  lastSentAt: string | null
}

const EVENT_LABELS: Record<string, () => string> = {
  'issue.created': () => t('Issue created'),
  'issue.updated': () => t('Issue updated'),
  'issue.deleted': () => t('Issue deleted'),
  'comment.created': () => t('Comment added'),
  'sprint.started': () => t('Sprint started'),
  'sprint.completed': () => t('Sprint completed'),
  'version.released': () => t('Version released'),
}
const eventLabel = (e: string) => EVENT_LABELS[e]?.() ?? e
const allEvents = (ev: string[]) => ev.length === 0 || ev.includes('*')

function EventPicker({ events, value, onChange }: { events: string[]; value: string[]; onChange: (v: string[]) => void }) {
  const all = allEvents(value)
  return (
    <div className="adm-events">
      <label className="adm-event adm-event-all">
        <input type="checkbox" checked={all} onChange={(e) => onChange(e.target.checked ? ['*'] : [])} />
        <span><strong>{t('All events')}</strong> <span className="faint small">{t('including ones added in later versions')}</span></span>
      </label>
      {events.map((ev) => (
        <label key={ev} className={`adm-event ${all ? 'adm-dim' : ''}`}>
          <input
            type="checkbox"
            disabled={all}
            checked={all || value.includes(ev)}
            onChange={(e) => onChange(e.target.checked ? [...value.filter((x) => x !== '*'), ev] : value.filter((x) => x !== ev))}
          />
          <span>{eventLabel(ev)}</span>
          <code className="adm-event-code">{ev}</code>
        </label>
      ))}
    </div>
  )
}

function SignatureHelp() {
  return (
    <div className="adm-sig small">
      <p>
        {t('Each delivery is a JSON POST with an X-TaskCraft-Event header and a signature: the HMAC-SHA256 of the raw request body, keyed with the secret.')}
      </p>
      <pre className="st-code">{`X-TaskCraft-Signature: sha256=<hex HMAC-SHA256(secret, body)>\n\n// Node.js\nconst expected = 'sha256=' + crypto.createHmac('sha256', secret).update(rawBody).digest('hex')\ncrypto.timingSafeEqual(Buffer.from(expected), Buffer.from(header))`}</pre>
    </div>
  )
}

function WebhooksTab() {
  const qc = useQueryClient()
  const toast = useToast()
  const hooks = useQuery({ queryKey: ['admin', 'webhooks'], queryFn: () => api.get<{ events: string[]; webhooks: Hook[] }>('/admin/webhooks') })
  const [adding, setAdding] = useState(false)
  const [editing, setEditing] = useState<Hook | null>(null)
  const [deleting, setDeleting] = useState<Hook | null>(null)
  const [testing, setTesting] = useState<string | null>(null)
  const [tests, setTests] = useState<Record<string, { ok: boolean; status?: number; error?: string }>>({})
  const refresh = () => qc.invalidateQueries({ queryKey: ['admin', 'webhooks'] })
  const events = hooks.data?.events ?? []
  const list = hooks.data?.webhooks ?? []
  const test = async (h: Hook) => {
    setTesting(h.id)
    try {
      const r = await api.post<{ ok: boolean; status?: number; error?: string }>(`/admin/webhooks/${h.id}/test`)
      setTests((m) => ({ ...m, [h.id]: r }))
      toast(r.ok ? t('Test delivered (HTTP {status})', { status: r.status ?? '' }) : t('Test failed: {error}', { error: r.error ?? '' }))
      refresh()
    } catch (e) {
      toast((e as Error).message)
    } finally {
      setTesting(null)
    }
  }
  const setActive = async (h: Hook, active: boolean) => {
    qc.setQueryData<{ events: string[]; webhooks: Hook[] }>(['admin', 'webhooks'], (d) => d && { ...d, webhooks: d.webhooks.map((x) => (x.id === h.id ? { ...x, active } : x)) })
    try {
      await api.patch(`/admin/webhooks/${h.id}`, { active })
    } catch (e) {
      toast((e as Error).message)
    }
    refresh()
  }
  return (
    <>
      <div className="adm-toolbar">
        <p className="small muted adm-intro">
          {t('Webhooks send a signed JSON POST to another service when issues, comments, sprints or versions change. Failed deliveries are retried.')}
        </p>
        <span className="spacer" />
        <button className="btn btn-primary" onClick={() => setAdding(true)}><Plus size={15} /> {t('Add webhook')}</button>
      </div>
      {hooks.isLoading && <Loading />}
      {hooks.error && <ErrorBox error={hooks.error} />}
      {hooks.data && list.length === 0 && (
        <Empty icon={<Webhook size={32} />} title={t('No webhooks yet')}>{t('Connect chat tools, CI or your own services to what happens in TaskCraft.')}</Empty>
      )}
      <div className="adm-hooks">
        {list.map((h) => {
          const res = tests[h.id]
          const failed = !!h.lastError || (h.lastStatus != null && (h.lastStatus < 200 || h.lastStatus >= 300))
          return (
            <div key={h.id} className={`adm-hook ${h.active ? '' : 'adm-off'}`}>
              <div className="adm-hook-head">
                <Switch checked={h.active} onChange={(on) => setActive(h, on)} label={h.active ? t('Active: switch off') : t('Paused: switch on')} />
                <span className="adm-hook-url" title={h.url}>{h.url}</span>
                <span className={`badge ${h.projectKey ? 'badge-accent' : ''}`}>{h.projectKey ? h.projectKey : t('All projects')}</span>
                {!h.active && <span className="badge badge-warning">{t('Paused')}</span>}
              </div>
              <div className="adm-hook-events">
                {allEvents(h.events) ? <span className="label-chip">{t('All events')}</span> : h.events.map((ev) => <span key={ev} className="label-chip" title={ev}>{eventLabel(ev)}</span>)}
              </div>
              <div className="adm-hook-foot">
                <span className={`small ${failed ? 'adm-bad' : h.lastSentAt ? 'adm-good' : 'faint'}`}>
                  {h.lastSentAt
                    ? failed
                      ? t('Last delivery failed {when}: {error}', { when: ago(h.lastSentAt), error: h.lastError ?? `HTTP ${h.lastStatus}` })
                      : t('Last delivered {when} (HTTP {status})', { when: ago(h.lastSentAt), status: h.lastStatus ?? '' })
                    : t('Nothing delivered yet')}
                </span>
                {res && (
                  <span className={`small ${res.ok ? 'adm-good' : 'adm-bad'}`} role="status">
                    {res.ok ? t('Test: HTTP {status}', { status: res.status ?? '' }) : t('Test failed: {error}', { error: res.error ?? '' })}
                  </span>
                )}
                <span className="spacer" />
                <button className="btn btn-sm" disabled={testing === h.id} onClick={() => test(h)}><Send size={13} /> {testing === h.id ? t('Sending…') : t('Send test')}</button>
                <button className="btn btn-sm" onClick={() => setEditing(h)}><Pencil size={13} /> {t('Events')}</button>
                <button className="icon-btn icon-btn-sm" title={t('Delete webhook')} aria-label={t('Delete webhook')} onClick={() => setDeleting(h)}><Trash size={14} /></button>
              </div>
            </div>
          )
        })}
      </div>
      {list.length > 0 && (
        <details className="adm-details">
          <summary>{t('Verifying deliveries')}</summary>
          <SignatureHelp />
        </details>
      )}
      {adding && <AddWebhook events={events} onClose={() => setAdding(false)} onCreated={refresh} />}
      {editing && <EditWebhook hook={editing} events={events} onClose={() => setEditing(null)} onSaved={refresh} />}
      {deleting && (
        <Confirm
          title={t('Delete this webhook?')}
          body={<p>{t('{url} stops receiving events.', { url: deleting.url })}</p>}
          confirmLabel={t('Delete webhook')}
          danger
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            try {
              await api.del(`/admin/webhooks/${deleting.id}`)
              refresh()
            } catch (e) {
              toast((e as Error).message)
            }
          }}
        />
      )}
    </>
  )
}

function AddWebhook({ events, onClose, onCreated }: { events: string[]; onClose: () => void; onCreated: () => void }) {
  const projects = useProjects()
  const [url, setUrl] = useState('')
  const [project, setProject] = useState('')
  const [chosen, setChosen] = useState<string[]>(['*'])
  const [secret, setSecret] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const [created, setCreated] = useState<string | null>(null)
  const adminProjects = (projects.data ?? []).filter((p) => p.role === 'admin' && !p.archivedAt)
  const submit = async () => {
    if (!/^https?:\/\/\S+$/i.test(url.trim())) return setErr(t('Enter an http:// or https:// address.'))
    if (!allEvents(chosen) && chosen.length === 0) return setErr(t('Pick at least one event.'))
    if (secret && secret.length < 8) return setErr(t('The secret needs at least 8 characters, or leave it empty.'))
    setBusy(true)
    setErr('')
    try {
      const r = await api.post<{ id: string; secret: string }>('/admin/webhooks', { url: url.trim(), events: chosen, projectKey: project || undefined, secret: secret || undefined })
      onCreated()
      setCreated(r.secret)
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  if (created) {
    return (
      <Modal title={t('Webhook added')} onClose={onClose} width={600} footer={<button className="btn btn-primary" onClick={onClose}>{t('Done [[close]]')}</button>}>
        <p>{t('Copy the signing secret now: it isn’t shown again. Your service uses it to check that deliveries really come from TaskCraft.')}</p>
        <CopyField value={created} label={t('Signing secret')} />
        <SignatureHelp />
      </Modal>
    )
  }
  return (
    <Modal
      title={t('Add webhook')}
      onClose={onClose}
      width={600}
      footer={
        <>
          {err && <span className="error-text adm-foot-msg" role="alert">{err}</span>}
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !url.trim()} onClick={submit}>{t('Add webhook')}</button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="adm-hook-url">{t('Payload URL')}</label>
        <input id="adm-hook-url" className="input" autoFocus type="url" value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://example.com/hooks/taskcraft" spellCheck={false} />
      </div>
      <div className="field">
        <label htmlFor="adm-hook-project">{t('Project')}</label>
        <select id="adm-hook-project" className="select" value={project} onChange={(e) => setProject(e.target.value)}>
          <option value="">{t('All projects')}</option>
          {adminProjects.map((p) => <option key={p.id} value={p.key}>{p.name} ({p.key})</option>)}
        </select>
      </div>
      <div className="field">
        <span className="field-label">{t('Events')}</span>
        <EventPicker events={events} value={chosen} onChange={setChosen} />
      </div>
      <div className="field">
        <label htmlFor="adm-hook-secret">{t('Secret (optional)')}</label>
        <input id="adm-hook-secret" className="input" value={secret} onChange={(e) => setSecret(e.target.value)} placeholder={t('Leave empty to generate one')} autoComplete="off" spellCheck={false} />
        <span className="field-hint">{t('Used to sign each delivery (X-TaskCraft-Signature: sha256=…).')}</span>
      </div>
    </Modal>
  )
}

function EditWebhook({ hook, events, onClose, onSaved }: { hook: Hook; events: string[]; onClose: () => void; onSaved: () => void }) {
  const [chosen, setChosen] = useState<string[]>(allEvents(hook.events) ? ['*'] : hook.events)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const save = async () => {
    if (!allEvents(chosen) && chosen.length === 0) return setErr(t('Pick at least one event.'))
    setBusy(true)
    try {
      await api.patch(`/admin/webhooks/${hook.id}`, { events: chosen })
      onSaved()
      onClose()
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal
      title={t('Webhook events')}
      onClose={onClose}
      width={540}
      footer={
        <>
          {err && <span className="error-text adm-foot-msg" role="alert">{err}</span>}
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy} onClick={save}>{t('Save')}</button>
        </>
      }
    >
      <p className="small muted adm-ellipsis" title={hook.url}>{hook.url}</p>
      <EventPicker events={events} value={chosen} onChange={setChosen} />
    </Modal>
  )
}

// ---- audit log ----

interface AuditEntry {
  id: string
  actor: string | null
  action: string
  target: string | null
  data: unknown
  at: string
}

function details(data: unknown): string {
  if (data == null) return ''
  if (typeof data === 'object' && Object.keys(data as object).length === 0) return ''
  const s = JSON.stringify(data)
  return s.length > 240 ? s.slice(0, 240) + '…' : s
}

function AuditTab() {
  const audit = useQuery({ queryKey: ['admin', 'audit'], queryFn: () => api.get<AuditEntry[]>('/admin/audit') })
  const [q, setQ] = useState('')
  const needle = q.trim().toLowerCase()
  const all = audit.data ?? []
  const list = needle ? all.filter((a) => `${a.actor ?? ''} ${a.action} ${a.target ?? ''} ${JSON.stringify(a.data ?? '')}`.toLowerCase().includes(needle)) : all
  return (
    <>
      <div className="adm-toolbar">
        <div className="adm-search adm-search-wide">
          <Search size={15} aria-hidden />
          <input className="input" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t('Filter by person, action or target…')} aria-label={t('Filter the audit log')} />
        </div>
        <span className="spacer" />
        <span className="faint small">
          {needle ? t('{shown} of {total} entries', { shown: list.length, total: all.length }) : t('The latest {count} entries', { count: all.length })}
        </span>
      </div>
      {audit.isLoading && <Loading />}
      {audit.error && <ErrorBox error={audit.error} />}
      {audit.data && list.length === 0 && <Empty icon={<ScrollText size={32} />} title={needle ? t('Nothing matches “{q}”', { q }) : t('Nothing logged yet')} />}
      {list.length > 0 && (
        <div className="adm-table-wrap">
          <table className="list adm-table adm-audit">
            <thead>
              <tr>
                <th>{t('When [[time]]')}</th>
                <th>{t('Who')}</th>
                <th>{t('Action')}</th>
                <th>{t('Target')}</th>
                <th>{t('Details')}</th>
              </tr>
            </thead>
            <tbody>
              {list.map((a) => (
                <tr key={a.id}>
                  <td className="small adm-nowrap" title={dateTime(a.at)}>{dateTime(a.at)}</td>
                  <td className="adm-nowrap">{a.actor ?? <span className="faint">{t('System')}</span>}</td>
                  <td><code className="adm-action">{a.action}</code></td>
                  <td className="small adm-target">{a.target}</td>
                  <td className="small adm-data"><code>{details(a.data)}</code></td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  )
}
