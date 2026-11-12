import { t } from '../i18n'
import { createContext, useCallback, useContext, useEffect, useState } from 'react'
import { Link, NavLink, Outlet, useLocation, useMatch, useNavigate, useSearchParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Search, House, FolderKanban, Plus, Moon, Shield, Menu, LogOut, UserCog, Bell, ListFilter, Timer, Users, Kanban, ListTodo, Table, ChartGantt, Calendar, ChartColumn,
  Package, BookOpen, Settings, Star, X, ExternalLink,
} from 'lucide-react'
import { api, ago, type Notification, type SavedView } from '../api'
import { Avatar, BotAvatar, Logo, Popover, useMe, useProject, useProjects, useHotkeys } from '../ui'
import { ProjectIcon } from '../icons'
import CreateIssue, { type CreateDefaults } from './CreateIssue'
import CommandPalette from './CommandPalette'
import IssueView from './IssueView'

const CreateCtx = createContext<(d?: CreateDefaults) => void>(() => {})
/** Opens the "create issue" dialog with defaults (project, status, sprint, …). */
export const useCreateIssue = () => useContext(CreateCtx)

/** Opens an issue in the side panel (keeps the page underneath). */
export function useOpenIssue() {
  const [params, setParams] = useSearchParams()
  return useCallback(
    (key: string | null) => {
      const p = new URLSearchParams(params)
      if (key) p.set('issue', key)
      else p.delete('issue')
      setParams(p, { replace: false })
    },
    [params, setParams],
  )
}

export function toggleTheme() {
  const cur = document.documentElement.dataset.theme
  const isDark = cur === 'dark' || (!cur && matchMedia('(prefers-color-scheme: dark)').matches)
  const next = isDark ? 'light' : 'dark'
  document.documentElement.dataset.theme = next
  try {
    localStorage.setItem('tc-theme', next)
  } catch {
    /* ignore */
  }
}

export default function Layout() {
  const me = useMe()
  const user = me.data?.user
  const qc = useQueryClient()
  const loc = useLocation()
  const [params] = useSearchParams()
  const openIssue = useOpenIssue()
  const [palette, setPalette] = useState(false)
  const [create, setCreate] = useState<CreateDefaults | null>(null)
  const [mobileOpen, setMobileOpen] = useState(false)
  const projectMatch = useMatch('/projects/:key/*')
  const projectKey = projectMatch?.params.key
  const project = useProject(projectKey)
  const projects = useProjects()
  const views = useQuery({ queryKey: ['views'], queryFn: () => api.get<SavedView[]>('/views') })
  const issueKey = params.get('issue')

  useEffect(() => setMobileOpen(false), [loc.pathname])
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault()
        setPalette(true)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])
  useHotkeys(
    {
      c: () => setCreate({ projectKey: project.data?.key }),
      '/': () => setPalette(true),
    },
    [project.data?.key],
  )
  const startCreate = useCallback((d?: CreateDefaults) => setCreate({ projectKey: project.data?.key, ...d }), [project.data?.key])
  const logout = async () => {
    await api.post('/auth/logout')
    qc.clear()
    location.href = '/login'
  }
  const isAdmin = user?.role === 'owner' || user?.role === 'admin'
  const starred = (projects.data ?? []).filter((p) => p.starred && !p.archivedAt)
  const p = project.data
  const pk = p?.key ?? projectKey?.toUpperCase()

  return (
    <CreateCtx.Provider value={startCreate}>
      <div className={`shell ${mobileOpen ? 'sidebar-open' : ''}`}>
        <header className="topbar">
          <button className="icon-btn mobile-only" onClick={() => setMobileOpen((o) => !o)} aria-label={t('Menu')}>
            <Menu size={18} />
          </button>
          <Link to="/" className="logo-link">
            <Logo />
          </Link>
          <button className="search-trigger" onClick={() => setPalette(true)}>
            <Search size={15} /> <span className="search-label">{t('Search issues, projects, actions…')}</span> <span className="spacer" /> <span className="kbd">Ctrl K</span>
          </button>
          <span className="spacer" />
          <button className="btn btn-primary" onClick={() => startCreate()} title={t('Create issue (C)')}>
            <Plus size={15} /> <span className="create-label">{t('Create')}</span>
          </button>
          <Notifications />
          <button className="icon-btn theme-btn" onClick={toggleTheme} title={t('Toggle dark mode')} aria-label={t('Toggle dark mode')}>
            <Moon size={16} />
          </button>
          {user && <UserMenu name={user.name} id={user.id} email={user.email} isAdmin={isAdmin} onLogout={logout} />}
        </header>

        <aside className="sidebar">
          {projectKey ? (
            <>
              <Link to="/projects" className="nav-item faint" style={{ fontSize: 12.5, height: 26 }}>← {t('All projects')}</Link>
              <div className="project-switch">
                <ProjectIcon icon={p?.icon} color={p?.color ?? 'var(--border-strong)'} size={30} />
                <div style={{ minWidth: 0 }}>
                  <div className="sw-name">{p?.name ?? projectKey.toUpperCase()}</div>
                  <div className="sw-kind">{p ? `${p.kind === 'scrum' ? t('Scrum project') : p.kind === 'kanban' ? t('Kanban project') : t('Tracker')} · ${p.key}` : '\u00a0'}</div>
                </div>
              </div>
              {(!p || p.kind === 'scrum') && <ProjectLink to={`/projects/${pk}/backlog`} icon={<ListTodo size={16} />} label={t('Backlog')} />}
              <ProjectLink to={`/projects/${pk}/board`} icon={<Kanban size={16} />} label={p && p.kind !== 'scrum' ? t('Board') : t('Active sprint')} />
              <ProjectLink to={`/projects/${pk}/list`} icon={<Table size={16} />} label={t('List')} />
              <ProjectLink to={`/projects/${pk}/timeline`} icon={<ChartGantt size={16} />} label={t('Timeline')} />
              <ProjectLink to={`/projects/${pk}/calendar`} icon={<Calendar size={16} />} label={t('Calendar')} />
              <ProjectLink to={`/projects/${pk}/reports`} icon={<ChartColumn size={16} />} label={t('Reports')} />
              <ProjectLink to={`/projects/${pk}/releases`} icon={<Package size={16} />} label={t('Releases')} />
              {me.data?.wikicraft?.url && <ProjectLink to={`/projects/${pk}/docs`} icon={<BookOpen size={16} />} label={t('Docs')} />}
              {p?.role === 'admin' && <ProjectLink to={`/projects/${pk}/settings`} icon={<Settings size={16} />} label={t('Project settings')} />}
            </>
          ) : (
            <>
              <NavLink to="/" end className="nav-item"><House size={16} /> <span className="nav-label">{t('Your work')}</span></NavLink>
              <NavLink to="/projects" className="nav-item"><FolderKanban size={16} /> <span className="nav-label">{t('Projects')}</span></NavLink>
              <NavLink to="/search" className="nav-item"><ListFilter size={16} /> <span className="nav-label">{t('Filters')}</span></NavLink>
              <NavLink to="/timesheet" className="nav-item"><Timer size={16} /> <span className="nav-label">{t('Timesheet')}</span></NavLink>
              <NavLink to="/people" className="nav-item"><Users size={16} /> <span className="nav-label">{t('People & teams')}</span></NavLink>
              {isAdmin && <NavLink to="/admin" className="nav-item"><Shield size={16} /> <span className="nav-label">{t('Administration')}</span></NavLink>}
            </>
          )}
          {starred.length > 0 && (
            <>
              <div className="sidebar-section">{t('Starred projects')}</div>
              {starred.map((sp) => (
                <NavLink key={sp.id} to={`/projects/${sp.key}`} className={() => `nav-item ${sp.key === p?.key ? 'active' : ''}`}>
                  <ProjectIcon icon={sp.icon} color={sp.color} size={18} />
                  <span className="nav-label">{sp.name}</span>
                  {!!sp.openCount && <span className="nav-count">{sp.openCount}</span>}
                </NavLink>
              ))}
            </>
          )}
          {(views.data ?? []).some((v) => v.starred) && (
            <>
              <div className="sidebar-section">{t('Saved filters')}</div>
              {(views.data ?? []).filter((v) => v.starred).map((v) => (
                <NavLink key={v.id} to={`/search?view=${v.id}`} className={() => `nav-item ${params.get('view') === v.id ? 'active' : ''}`}>
                  <Star size={14} /> <span className="nav-label">{v.name}</span>
                </NavLink>
              ))}
            </>
          )}
        </aside>

        <main className="main" id="main">
          <Outlet />
        </main>

        {issueKey && (
          <>
            <div className="issue-panel-backdrop" onClick={() => openIssue(null)} />
            <IssuePanel issueKey={issueKey} onClose={() => openIssue(null)} onOpen={(k) => openIssue(k)} />
          </>
        )}
        {palette && <CommandPalette onClose={() => setPalette(false)} onCreate={() => { setPalette(false); startCreate() }} />}
        {create && <CreateIssue defaults={create} onClose={() => setCreate(null)} />}
      </div>
    </CreateCtx.Provider>
  )
}

function ProjectLink({ to, icon, label }: { to: string; icon: React.ReactNode; label: string }) {
  return (
    <NavLink to={to} className="nav-item">
      {icon} <span className="nav-label">{label}</span>
    </NavLink>
  )
}

function IssuePanel({ issueKey, onClose, onOpen }: { issueKey: string; onClose: () => void; onOpen: (k: string) => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || document.querySelector('.modal-backdrop, .popover, .suggest-pop')) return
      const el = e.target as HTMLElement
      if (el?.closest('[contenteditable="true"], input, textarea')) return
      onClose()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])
  return (
    <section className="issue-panel" aria-label={issueKey}>
      <div className="issue-panel-head">
        <span className="issue-key">{issueKey}</span>
        <span className="spacer" />
        <Link to={`/issues/${issueKey}`} className="icon-btn" title={t('Open as page')} aria-label={t('Open as page')}><ExternalLink size={15} /></Link>
        <button className="icon-btn" onClick={onClose} aria-label={t('Close')}><X size={16} /></button>
      </div>
      <IssueView key={issueKey} issueKey={issueKey} onOpen={onOpen} inPanel />
    </section>
  )
}

function UserMenu({ name, id, email, isAdmin, onLogout }: { name: string; id: string; email: string | null; isAdmin: boolean; onLogout: () => void }) {
  const [el, setEl] = useState<HTMLButtonElement | null>(null)
  const [open, setOpen] = useState(false)
  const nav = useNavigate()
  const close = useCallback(() => setOpen(false), [])
  return (
    <>
      <button ref={setEl} className="icon-btn" style={{ width: 'auto', padding: '0 4px' }} onClick={() => setOpen((o) => !o)} aria-label={t('Account')}>
        <Avatar name={name} id={id} size={26} />
      </button>
      {open && (
        <Popover anchor={el} onClose={close} align="end" width={240}>
          <div style={{ padding: '6px 8px 8px' }}>
            <div style={{ fontWeight: 600 }}>{name}</div>
            <div className="faint small">{email}</div>
          </div>
          <div className="divider" />
          <button className="menu-item" onClick={() => { close(); nav('/settings') }}><UserCog size={15} /> {t('Profile & settings')}</button>
          <button className="menu-item" onClick={() => { close(); nav('/timesheet') }}><Timer size={15} /> {t('My timesheet')}</button>
          {isAdmin && <button className="menu-item" onClick={() => { close(); nav('/admin') }}><Shield size={15} /> {t('Administration')}</button>}
          <button className="menu-item" onClick={onLogout}><LogOut size={15} /> {t('Log out')}</button>
        </Popover>
      )}
    </>
  )
}

function Notifications() {
  const [el, setEl] = useState<HTMLButtonElement | null>(null)
  const [open, setOpen] = useState(false)
  const qc = useQueryClient()
  const openIssue = useOpenIssue()
  const n = useQuery({ queryKey: ['notifications'], queryFn: () => api.get<{ unread: number; items: Notification[] }>('/notifications'), refetchInterval: 120_000 })
  const close = useCallback(() => setOpen(false), [])
  const text = (x: Notification) => {
    const who = x.actor?.name ?? t('Automation')
    switch (x.kind) {
      case 'assigned':
        return t('{who} assigned you', { who })
      case 'mentioned':
        return t('{who} mentioned you', { who })
      case 'commented':
        return t('{who} commented', { who })
      case 'status':
        return t('{who} moved it to {to}', { who, to: x.data.to ?? '' })
      default:
        return who
    }
  }
  const unread = n.data?.unread ?? 0
  return (
    <>
      <button ref={setEl} className="icon-btn" style={{ position: 'relative' }} onClick={() => setOpen((o) => !o)} aria-label={unread ? t('Notifications: {n} unread', { n: unread > 99 ? '99+' : unread }) : t('Notifications')} title={t('Notifications')}>
        <Bell size={16} />
        {unread > 0 && <span className="notif-dot" data-count={unread > 99 ? '99+' : unread} aria-hidden />}
      </button>
      {open && (
        <Popover anchor={el} onClose={close} align="end" width={380}>
          <div className="row" style={{ padding: '6px 8px' }}>
            <strong>{t('Notifications')}</strong>
            <span className="spacer" />
            {unread > 0 && (
              <button className="btn btn-ghost btn-sm" onClick={async () => { await api.post('/notifications/read', { all: true }); qc.invalidateQueries({ queryKey: ['notifications'] }) }}>{t('Mark all read')}</button>
            )}
          </div>
          <div className="divider" />
          {(n.data?.items ?? []).length === 0 && <div className="menu-empty" style={{ padding: 20, textAlign: 'center' }}>{t('Nothing new. Assignments, mentions and changes to issues you watch show up here.')}</div>}
          {(n.data?.items ?? []).slice(0, 40).map((x) => (
            <button
              key={x.id}
              className="menu-item"
              style={{ alignItems: 'flex-start', opacity: x.read ? 0.7 : 1 }}
              onClick={async () => {
                close()
                if (!x.read) {
                  await api.post('/notifications/read', { ids: [x.id] })
                  qc.invalidateQueries({ queryKey: ['notifications'] })
                }
                openIssue(x.data.key)
              }}
            >
              {x.actor ? <Avatar name={x.actor.name} id={x.actor.id} size={26} /> : <BotAvatar size={26} />}
              <span style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontWeight: x.read ? 400 : 600 }}>{text(x)}</div>
                <div className="small" style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  <span className="issue-key">{x.data.key}</span> {x.data.title}
                </div>
                {x.data.excerpt && <div className="mi-desc" style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{x.data.excerpt}</div>}
                <div className="mi-desc">{ago(x.createdAt)}</div>
              </span>
              {!x.read && <span className="dot" style={{ background: 'var(--accent)', marginTop: 8 }} />}
            </button>
          ))}
        </Popover>
      )}
    </>
  )
}
