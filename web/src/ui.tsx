import { t } from './i18n'
import { createContext, useCallback, useContext, useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'
import { useQuery } from '@tanstack/react-query'
import { Bot, X } from 'lucide-react'
import { api, initials, userColor, type Category, type Me, type Person, type Project, type ProjectSummary } from './api'

export function useMe() {
  return useQuery({ queryKey: ['me'], queryFn: () => api.get<Me>('/me'), staleTime: 60_000 })
}

/** Whether the workspace still needs setting up, and what this server offers (`mail`: e-mail is configured). */
export function useSetup() {
  return useQuery({ queryKey: ['setup'], queryFn: () => api.get<{ needsSetup: boolean; mail?: boolean }>('/setup') })
}

export function usePeople() {
  const me = useMe()
  return useQuery({ queryKey: ['people'], queryFn: () => api.get<Person[]>('/people'), enabled: !!me.data?.user, staleTime: 120_000 })
}

export function useProjects() {
  const me = useMe()
  return useQuery({ queryKey: ['projects'], queryFn: () => api.get<ProjectSummary[]>('/projects'), enabled: !!me.data?.user })
}

export function useProject(key: string | undefined) {
  return useQuery({ queryKey: ['project', key?.toUpperCase()], queryFn: () => api.get<Project>(`/projects/${encodeURIComponent(key!)}`), enabled: !!key, staleTime: 60_000 })
}

export function Logo({ name }: { name?: string }) {
  return (
    <span className="logo">
      <svg viewBox="0 0 32 32" aria-hidden>
        <rect width="32" height="32" rx="7" fill="var(--accent)" />
        <rect x="7" y="8" width="5" height="16" rx="1.5" fill="#fff" />
        <rect x="13.5" y="8" width="5" height="11" rx="1.5" fill="#fff" opacity=".85" />
        <rect x="20" y="8" width="5" height="7" rx="1.5" fill="#fff" opacity=".7" />
      </svg>
      <span className="logo-text">{name ?? 'TaskCraft'}</span>
    </span>
  )
}

/** A person's initials on their colour. `decorative` when their name is written next to it. */
export function Avatar({ name, id, size = 24, decorative }: { name: string; id?: string; size?: number; decorative?: boolean }) {
  return (
    <span
      className="avatar"
      data-initials={initials(name)}
      style={{ background: userColor(id ?? name), width: size, height: size, fontSize: Math.max(9, size * 0.42) }}
      title={decorative ? undefined : name}
      aria-label={decorative ? undefined : name}
      aria-hidden={decorative || undefined}
      role={decorative ? undefined : 'img'}
    />
  )
}

/** The avatar of changes automation rules made. */
export function BotAvatar({ size = 24 }: { size?: number }) {
  return (
    <span className="avatar" style={{ background: 'var(--text-soft)', width: size, height: size }} title={t('Automation')} aria-label={t('Automation')} role="img">
      <Bot size={Math.round(size * 0.62)} color="var(--bg)" aria-hidden />
    </span>
  )
}

export function Unassigned({ size = 24, decorative }: { size?: number; decorative?: boolean }) {
  if (decorative) return <span className="avatar avatar-empty" style={{ width: size, height: size }} aria-hidden />
  return <span className="avatar avatar-empty" style={{ width: size, height: size }} title={t('Unassigned')} aria-label={t('Unassigned')} role="img" />
}

export function Lozenge({ name, category, color }: { name: string; category: Category; color?: string | null }) {
  return (
    <span className={`lozenge lozenge-${category}`} style={color ? ({ '--loz': color } as React.CSSProperties) : undefined}>
      {name}
    </span>
  )
}

export function Modal({ title, onClose, children, footer, width, className }: { title: ReactNode; onClose: () => void; children: ReactNode; footer?: ReactNode; width?: number; className?: string }) {
  const titleId = useId()
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        onClose()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [onClose])
  return createPortal(
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className={`modal ${className ?? ''}`} style={width ? { width: `min(${width}px, calc(100vw - 32px))` } : undefined} role="dialog" aria-modal aria-labelledby={titleId}>
        <div className="modal-header">
          <div className="modal-title" id={titleId}>{title}</div>
          <span className="spacer" />
          <button className="icon-btn" onClick={onClose} aria-label={t('Close')}>
            <X size={16} />
          </button>
        </div>
        <div className="modal-body">{children}</div>
        {footer && <div className="modal-footer">{footer}</div>}
      </div>
    </div>,
    document.body,
  )
}

/** A floating panel under (or above) an anchor element; closes on outside click and Escape. */
export function Popover({ anchor, onClose, children, width, align = 'start', className }: { anchor: HTMLElement | null; onClose: () => void; children: ReactNode; width?: number; align?: 'start' | 'end'; className?: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const [pos, setPos] = useState<{ top: number; left: number; maxH: number } | null>(null)
  useLayoutEffect(() => {
    if (!anchor) return
    const place = () => {
      const r = anchor.getBoundingClientRect()
      const w = width ?? ref.current?.offsetWidth ?? 240
      let left = align === 'end' ? r.right - w : r.left
      left = Math.max(8, Math.min(left, window.innerWidth - w - 8))
      const below = window.innerHeight - r.bottom - 12
      const above = r.top - 12
      const h = ref.current?.offsetHeight ?? 300
      if (below < Math.min(h, 260) && above > below) setPos({ top: Math.max(8, r.top - 4 - Math.min(h, above)), left, maxH: above })
      else setPos({ top: r.bottom + 4, left, maxH: below })
    }
    place()
    window.addEventListener('resize', place)
    window.addEventListener('scroll', place, true)
    return () => {
      window.removeEventListener('resize', place)
      window.removeEventListener('scroll', place, true)
    }
  }, [anchor, width, align])
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (ref.current?.contains(e.target as Node) || anchor?.contains(e.target as Node)) return
      onClose()
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        onClose()
      }
    }
    document.addEventListener('mousedown', onDown)
    window.addEventListener('keydown', onKey, true)
    return () => {
      document.removeEventListener('mousedown', onDown)
      window.removeEventListener('keydown', onKey, true)
    }
  }, [anchor, onClose])
  return createPortal(
    <div
      ref={ref}
      className={`popover ${className ?? ''}`}
      style={{ top: pos?.top ?? -9999, left: pos?.left ?? -9999, width, maxHeight: pos ? Math.min(pos.maxH, 440) : undefined }}
      role="dialog"
    >
      {children}
    </div>,
    document.body,
  )
}

/** A searchable list of choices (pickers for status, people, sprint, …). */
export function Choices<T>({
  items,
  render,
  onPick,
  filterText,
  placeholder,
  selected,
  empty,
  label,
  multiple,
}: {
  items: T[]
  render: (item: T) => ReactNode
  onPick: (item: T) => void
  filterText: (item: T) => string
  placeholder?: string
  selected?: (item: T) => boolean
  empty?: string
  /** what the list is for (screen readers) */
  label?: string
  /** several may be chosen */
  multiple?: boolean
}) {
  const [q, setQ] = useState('')
  const [active, setActive] = useState(0)
  const list = items.filter((i) => filterText(i).toLowerCase().includes(q.trim().toLowerCase()))
  useEffect(() => setActive(0), [q])
  return (
    <div className="choices">
      {items.length > 6 && (
        <input
          className="input input-sm choices-search"
          autoFocus
          value={q}
          placeholder={placeholder ?? t('Search…')}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown') {
              e.preventDefault()
              setActive((a) => Math.min(a + 1, list.length - 1))
            } else if (e.key === 'ArrowUp') {
              e.preventDefault()
              setActive((a) => Math.max(a - 1, 0))
            } else if (e.key === 'Enter' && list[active]) {
              e.preventDefault()
              onPick(list[active])
            }
          }}
        />
      )}
      <div className="choices-list" role="listbox" aria-label={label ?? placeholder ?? t('Choices')} aria-multiselectable={multiple || undefined}>
        {list.map((item, i) => (
          <button
            type="button"
            key={i}
            role="option"
            aria-selected={selected?.(item) ?? false}
            className={`menu-item ${i === active ? 'hover' : ''} ${selected?.(item) ? 'selected' : ''}`}
            onMouseEnter={() => setActive(i)}
            onClick={() => onPick(item)}
          >
            {render(item)}
          </button>
        ))}
        {list.length === 0 && <div className="menu-empty">{empty ?? t('Nothing found')}</div>}
      </div>
    </div>
  )
}

/** A button that opens a popover. */
export function PopButton({ label, children, className, title, width, align, disabled }: { label: ReactNode; children: (close: () => void) => ReactNode; className?: string; title?: string; width?: number; align?: 'start' | 'end'; disabled?: boolean }) {
  const [el, setEl] = useState<HTMLButtonElement | null>(null)
  const [open, setOpen] = useState(false)
  const close = useCallback(() => setOpen(false), [])
  const id = useId()
  return (
    <>
      {/* the name is the field ("Assignee") followed by what the button shows ("Jonas Weber") */}
      {title && <span id={`${id}-t`} hidden>{title}</span>}
      <button
        type="button"
        id={id}
        ref={setEl}
        className={className ?? 'btn'}
        title={title}
        aria-labelledby={title ? `${id}-t ${id}` : undefined}
        aria-haspopup="dialog"
        aria-expanded={open}
        disabled={disabled}
        onClick={() => setOpen((o) => !o)}
      >
        {label}
      </button>
      {open && (
        <Popover anchor={el} onClose={close} width={width} align={align}>
          {children(close)}
        </Popover>
      )}
    </>
  )
}

const ToastCtx = createContext<(msg: string, action?: { label: string; run: () => void }) => void>(() => {})

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toast, setToast] = useState<{ msg: string; action?: { label: string; run: () => void } } | null>(null)
  useEffect(() => {
    if (!toast) return
    const h = setTimeout(() => setToast(null), toast.action ? 6000 : 3000)
    return () => clearTimeout(h)
  }, [toast])
  const show = useCallback((msg: string, action?: { label: string; run: () => void }) => setToast({ msg, action }), [])
  return (
    <ToastCtx.Provider value={show}>
      {children}
      {toast && (
        <div className="toast" role="status">
          {toast.msg}
          {toast.action && (
            <button
              className="toast-action"
              onClick={() => {
                toast.action!.run()
                setToast(null)
              }}
            >
              {toast.action.label}
            </button>
          )}
        </div>
      )}
    </ToastCtx.Provider>
  )
}

export const useToast = () => useContext(ToastCtx)

export function Spinner({ size = 16 }: { size?: number }) {
  return <div className="spinner" style={{ width: size, height: size }} role="status" aria-label={t('Loading')} />
}

export function Loading() {
  return (
    <div className="loading">
      <Spinner />
    </div>
  )
}

export function ErrorBox({ error }: { error: unknown }) {
  const msg = error instanceof Error ? error.message : String(error)
  return <div className="card error-text">{msg}</div>
}

export function Empty({ icon, title, children }: { icon?: ReactNode; title: string; children?: ReactNode }) {
  return (
    <div className="empty">
      {icon && <div className="empty-icon">{icon}</div>}
      <div className="empty-title">{title}</div>
      {children && <div className="empty-body">{children}</div>}
    </div>
  )
}

/** Keyboard shortcut handler that ignores typing in inputs and editors. */
export function useHotkeys(map: Record<string, (e: KeyboardEvent) => void>, deps: unknown[] = []) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement
      if (el?.closest('input, textarea, select, [contenteditable="true"]') || e.metaKey || e.ctrlKey || e.altKey) return
      if (document.querySelector('.modal-backdrop, .popover')) return
      const f = map[e.key]
      if (f) {
        e.preventDefault()
        f(e)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps)
}

export function Tabs<T extends string>({ value, onChange, tabs }: { value: T; onChange: (v: T) => void; tabs: { id: T; label: ReactNode }[] }) {
  return (
    <div className="tabs" role="tablist">
      {tabs.map((tab) => (
        <button key={tab.id} role="tab" aria-selected={value === tab.id} className={`tab ${value === tab.id ? 'active' : ''}`} onClick={() => onChange(tab.id)}>
          {tab.label}
        </button>
      ))}
    </div>
  )
}

/** Confirm dialog as a promise-free component. */
export function Confirm({ title, body, confirmLabel, danger, onConfirm, onClose }: { title: string; body: ReactNode; confirmLabel: string; danger?: boolean; onConfirm: () => void | Promise<void>; onClose: () => void }) {
  const [busy, setBusy] = useState(false)
  return (
    <Modal
      title={title}
      onClose={onClose}
      width={440}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button
            className={`btn ${danger ? 'btn-danger-solid' : 'btn-primary'}`}
            disabled={busy}
            autoFocus
            onClick={async () => {
              setBusy(true)
              try {
                await onConfirm()
                onClose()
              } finally {
                setBusy(false)
              }
            }}
          >
            {confirmLabel}
          </button>
        </>
      }
    >
      {body}
    </Modal>
  )
}
