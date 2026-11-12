import { t } from '../i18n'
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { useNavigate } from 'react-router-dom'
import { useQuery } from '@tanstack/react-query'
import { Search, Plus, House, FolderKanban, ListFilter, Timer, UserCog, Moon, Shield, Users, CornerDownLeft } from 'lucide-react'
import { api, tqlQuote, type Issue } from '../api'
import { Lozenge, useMe, useProjects } from '../ui'
import { ProjectIcon, TypeIcon } from '../icons'
import { toggleTheme, useOpenIssue } from './Layout'

interface Item {
  id: string
  group: string
  label: ReactNode
  hint?: ReactNode
  icon: ReactNode
  run: () => void
}

export default function CommandPalette({ onClose, onCreate }: { onClose: () => void; onCreate: () => void }) {
  const [q, setQ] = useState('')
  const [debounced, setDebounced] = useState('')
  const [sel, setSel] = useState(0)
  const nav = useNavigate()
  const openIssue = useOpenIssue()
  const projects = useProjects()
  const me = useMe().data
  const listRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const h = setTimeout(() => setDebounced(q), 120)
    return () => clearTimeout(h)
  }, [q])
  const issues = useQuery({ queryKey: ['quick', debounced], queryFn: () => api.get<Issue[]>(`/quick?q=${encodeURIComponent(debounced)}`) })
  const go = (f: () => void) => () => {
    onClose()
    f()
  }
  const items: Item[] = useMemo(() => {
    const ql = q.trim().toLowerCase()
    const out: Item[] = []
    for (const i of issues.data ?? []) {
      out.push({
        id: `i-${i.id}`,
        group: ql ? t('Issues') : t('Recently viewed'),
        icon: <TypeIcon icon={i.type.icon} color={i.type.color} />,
        label: (
          <>
            <span className="issue-key">{i.key}</span> <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{i.title}</span>
          </>
        ),
        hint: <Lozenge name={i.status.name} category={i.status.category} />,
        run: go(() => openIssue(i.key)),
      })
    }
    if (ql) {
      out.push({ id: 'search', group: t('Issues'), icon: <Search size={15} />, label: t('Search all issues for “{q}”', { q }), run: go(() => nav(`/search?q=${encodeURIComponent(`text ~ ${tqlQuote(q.trim())}`)}`)) })
    }
    for (const p of (projects.data ?? []).filter((p) => !ql || p.name.toLowerCase().includes(ql) || p.key.toLowerCase().includes(ql)).slice(0, ql ? 6 : 4)) {
      out.push({ id: `p-${p.id}`, group: t('Projects'), icon: <ProjectIcon icon={p.icon} color={p.color} size={18} />, label: p.name, hint: <span className="issue-key">{p.key}</span>, run: go(() => nav(`/projects/${p.key}`)) })
    }
    const actions: Item[] = [
      { id: 'create', group: t('Actions'), icon: <Plus size={15} />, label: t('Create issue'), hint: <span className="kbd">C</span>, run: () => onCreate() },
      { id: 'home', group: t('Actions'), icon: <House size={15} />, label: t('Go to your work'), run: go(() => nav('/')) },
      { id: 'projects', group: t('Actions'), icon: <FolderKanban size={15} />, label: t('Go to projects'), run: go(() => nav('/projects')) },
      { id: 'filters', group: t('Actions'), icon: <ListFilter size={15} />, label: t('Filter issues'), run: go(() => nav('/search')) },
      { id: 'timesheet', group: t('Actions'), icon: <Timer size={15} />, label: t('Open my timesheet'), run: go(() => nav('/timesheet')) },
      { id: 'people', group: t('Actions'), icon: <Users size={15} />, label: t('People & teams'), run: go(() => nav('/people')) },
      { id: 'settings', group: t('Actions'), icon: <UserCog size={15} />, label: t('Profile & settings'), run: go(() => nav('/settings')) },
      ...(me?.user?.role === 'owner' || me?.user?.role === 'admin' ? [{ id: 'admin', group: t('Actions'), icon: <Shield size={15} />, label: t('Administration'), run: go(() => nav('/admin')) }] : []),
      { id: 'theme', group: t('Actions'), icon: <Moon size={15} />, label: t('Toggle dark mode'), run: go(toggleTheme) },
    ]
    out.push(...actions.filter((a) => !ql || String(typeof a.label === 'string' ? a.label : '').toLowerCase().includes(ql)))
    return out
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [issues.data, projects.data, q, me])
  useEffect(() => setSel(0), [items.length, q])
  useEffect(() => {
    listRef.current?.querySelector('.menu-item.hover')?.scrollIntoView({ block: 'nearest' })
  }, [sel])
  let lastGroup = ''
  return createPortal(
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="palette" role="dialog" aria-modal aria-label={t('Search')}>
        <div className="palette-input">
          <Search size={17} className="faint" />
          <input
            autoFocus
            value={q}
            placeholder={t('Search issues by key or words, jump to a project, run an action…')}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') onClose()
              else if (e.key === 'ArrowDown') {
                e.preventDefault()
                setSel((s) => Math.min(s + 1, items.length - 1))
              } else if (e.key === 'ArrowUp') {
                e.preventDefault()
                setSel((s) => Math.max(s - 1, 0))
              } else if (e.key === 'Enter') {
                e.preventDefault()
                items[sel]?.run()
              }
            }}
          />
          <span className="kbd">Esc</span>
        </div>
        <div className="palette-list" ref={listRef}>
          {items.map((it, i) => {
            const head = it.group !== lastGroup ? <div className="menu-head">{it.group}</div> : null
            lastGroup = it.group
            return (
              <div key={it.id}>
                {head}
                <button className={`menu-item ${i === sel ? 'hover' : ''}`} onMouseEnter={() => setSel(i)} onClick={it.run}>
                  {it.icon}
                  <span className="row" style={{ flex: 1, minWidth: 0, gap: 6 }}>{it.label}</span>
                  {it.hint}
                  {i === sel && <CornerDownLeft size={13} className="faint" />}
                </button>
              </div>
            )
          })}
        </div>
      </div>
    </div>,
    document.body,
  )
}
