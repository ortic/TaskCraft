// Releases: the project's versions with progress from their issues, release / archive / delete,
// and release notes written to the WikiCraft space.
import { t } from '../i18n'
import { useState } from 'react'
import { useParams } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Package, Plus, ChevronDown, ChevronRight, Rocket, Ellipsis, Pencil, Archive, ArchiveArrowUp, Trash, BookOpen, Undo2, CalendarDays, TriangleExclamationPoint,
} from 'lucide-react'
import { api, daysUntil, qs, shortDate, type Issue, type Project, type Version } from '../api'
import { Avatar, Confirm, Empty, ErrorBox, Loading, Lozenge, Modal, PopButton, Unassigned, useMe, useProject, useToast } from '../ui'
import { TypeIcon } from '../icons'
import { useOpenIssue } from '../components/Layout'
import { refreshIssues } from '../components/IssueView'
import './Releases.css'

type SearchResult = { total: number; issues: Issue[] }

/** Always quoted: unquoted names like 1.0 would be read as the number 1. */
const quoted = (s: string) => `"${s.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`

const CATEGORY_ORDER: Record<string, number> = { in_progress: 0, todo: 1, done: 2 }

function useVersionIssues(projectKey: string, version: Version, enabled = true) {
  return useQuery({
    // under 'search' so live updates and issue edits refresh it
    queryKey: ['search', 'release-issues', projectKey, version.name],
    queryFn: () => api.get<SearchResult>(`/search${qs({ project: projectKey, limit: 1000, q: `version = ${quoted(version.name)}` })}`),
    enabled,
  })
}

function counts(issues: Issue[] | undefined) {
  const c = { total: 0, done: 0, progress: 0, todo: 0 }
  for (const i of issues ?? []) {
    c.total++
    if (i.status.category === 'done') c.done++
    else if (i.status.category === 'in_progress') c.progress++
    else c.todo++
  }
  return c
}

const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e))

function isOverdue(v: Version) {
  return v.status === 'unreleased' && !!v.releaseDate && daysUntil(v.releaseDate) < 0
}

export default function Releases() {
  const { key } = useParams()
  const project = useProject(key)
  const [showArchived, setShowArchived] = useState(false)
  const [editing, setEditing] = useState<Version | 'new' | null>(null)
  const [releasing, setReleasing] = useState<Version | null>(null)
  const [deleting, setDeleting] = useState<Version | null>(null)
  const qc = useQueryClient()
  const toast = useToast()

  if (project.isLoading) return <Loading />
  if (project.error) return <div className="page"><ErrorBox error={project.error} /></div>
  if (!project.data) return null
  const p = project.data
  const canEdit = p.role === 'member' || p.role === 'admin'
  const byDate = (a: Version, b: Version) => (a.releaseDate ?? '9999').localeCompare(b.releaseDate ?? '9999')
  const unreleased = p.versions.filter((v) => v.status === 'unreleased').sort(byDate)
  const released = p.versions.filter((v) => v.status === 'released').sort((a, b) => (b.releasedAt ?? b.releaseDate ?? '').localeCompare(a.releasedAt ?? a.releaseDate ?? ''))
  const archived = p.versions.filter((v) => v.status === 'archived')
  const overdue = unreleased.filter(isOverdue).length
  const next = unreleased.find((v) => v.releaseDate && !isOverdue(v))

  const card = (v: Version) => (
    <VersionCard
      key={v.id}
      project={p}
      version={v}
      canEdit={canEdit}
      onEdit={() => setEditing(v)}
      onRelease={() => setReleasing(v)}
      onDelete={() => setDeleting(v)}
    />
  )

  return (
    <div className="page rel-page">
      <div className="page-head">
        <h1><Package size={22} aria-hidden /> {t('Releases')}</h1>
        <span className="spacer" />
        {archived.length > 0 && (
          <button className={`btn ${showArchived ? 'active' : ''}`} aria-pressed={showArchived} onClick={() => setShowArchived((s) => !s)}>
            <Archive size={15} /> {t('Archived ({count})', { count: archived.length })}
          </button>
        )}
        {canEdit && (
          <button className="btn btn-primary" onClick={() => setEditing('new')}>
            <Plus size={15} /> {t('Create version')}
          </button>
        )}
      </div>

      {p.versions.length === 0 ? (
        <Empty icon={<Package size={34} />} title={t('No versions yet')}>
          {t('A version (release) groups the issues that ship together. Plan its dates, follow its progress and release it when it’s done.')}
          {canEdit && (
            <div style={{ marginTop: 12 }}>
              <button className="btn btn-primary" onClick={() => setEditing('new')}><Plus size={15} /> {t('Create version')}</button>
            </div>
          )}
        </Empty>
      ) : (
        <>
          {(unreleased.length > 0 || released.length > 0) && (
            <div className="rel-summary">
              <span>{t('{count} unreleased', { count: unreleased.length })}</span>
              <span>{t('{count} released', { count: released.length })}</span>
              {overdue > 0 && (
                <span className="rel-summary-overdue"><TriangleExclamationPoint size={14} aria-hidden /> {t('{count} overdue', { count: overdue })}</span>
              )}
              {next && next.releaseDate && <span>{t('Next: {name} on {date}', { name: next.name, date: shortDate(next.releaseDate) })}</span>}
            </div>
          )}

          <section className="rel-section">
            <h2 className="rel-section-title">{t('Unreleased')}</h2>
            {unreleased.length === 0 ? (
              <div className="rel-none">{t('Nothing planned. Create a version for the next release.')}</div>
            ) : (
              <div className="rel-list">{unreleased.map(card)}</div>
            )}
          </section>

          {released.length > 0 && (
            <section className="rel-section">
              <h2 className="rel-section-title">{t('Released')}</h2>
              <div className="rel-list">{released.map(card)}</div>
            </section>
          )}

          {showArchived && archived.length > 0 && (
            <section className="rel-section">
              <h2 className="rel-section-title">{t('Archived')}</h2>
              <div className="rel-list">{archived.map(card)}</div>
            </section>
          )}
        </>
      )}

      {editing && <VersionDialog project={p} version={editing === 'new' ? null : editing} onClose={() => setEditing(null)} />}
      {releasing && <ReleaseDialog project={p} version={releasing} onClose={() => setReleasing(null)} />}
      {deleting && (
        <Confirm
          title={t('Delete version {name}?', { name: deleting.name })}
          body={<p>{t('Its issues stay but lose their version. This can’t be undone.')}</p>}
          confirmLabel={t('Delete version')}
          danger
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            try {
              await api.del(`/versions/${deleting.id}`)
              qc.invalidateQueries({ queryKey: ['project', p.key] })
              refreshIssues(qc)
              toast(t('Version {name} deleted', { name: deleting.name }))
            } catch (e) {
              toast(errMsg(e))
            }
          }}
        />
      )}
    </div>
  )
}

function VersionCard({ project, version: v, canEdit, onEdit, onRelease, onDelete }: { project: Project; version: Version; canEdit: boolean; onEdit: () => void; onRelease: () => void; onDelete: () => void }) {
  const [open, setOpen] = useState(false)
  const issues = useVersionIssues(project.key, v)
  const c = counts(issues.data?.issues)
  const me = useMe().data
  const qc = useQueryClient()
  const toast = useToast()
  const openIssue = useOpenIssue()
  const [busy, setBusy] = useState(false)
  const overdue = isOverdue(v)
  const wiki = !!(me?.wikicraft?.connected && project.settings.wikiSpace)
  const pct = (n: number) => (c.total ? (n / c.total) * 100 : 0)
  const days = v.releaseDate ? daysUntil(v.releaseDate) : null

  const setStatus = async (status: Version['status'], msg: string) => {
    try {
      await api.patch(`/versions/${v.id}`, { status })
      qc.invalidateQueries({ queryKey: ['project', project.key] })
      toast(msg)
    } catch (e) {
      toast(errMsg(e))
    }
  }

  const writeNotes = async () => {
    setBusy(true)
    try {
      const page = await api.post<{ url: string }>(`/versions/${v.id}/wiki-page`, {})
      toast(t('Release notes written'), { label: t('Open [[action]]'), run: () => window.open(page.url, '_blank', 'noopener') })
    } catch (e) {
      toast(errMsg(e))
    } finally {
      setBusy(false)
    }
  }

  const sorted = [...(issues.data?.issues ?? [])].sort((a, b) => (CATEGORY_ORDER[a.status.category] ?? 1) - (CATEGORY_ORDER[b.status.category] ?? 1))
  const listId = `rel-issues-${v.id}`

  return (
    <article className={`rel-card ${overdue ? 'rel-overdue' : ''} ${v.status === 'archived' ? 'rel-archived' : ''}`}>
      <div className="rel-head">
        <button className="icon-btn icon-btn-sm rel-toggle" aria-expanded={open} aria-controls={listId} aria-label={open ? t('Hide issues') : t('Show issues')} title={open ? t('Hide issues') : t('Show issues')} onClick={() => setOpen((o) => !o)}>
          {open ? <ChevronDown size={16} /> : <ChevronRight size={16} />}
        </button>
        <div className="rel-title">
          <h3>
            <button className="rel-name" onClick={() => setOpen((o) => !o)}>{v.name}</button>
          </h3>
          {v.status === 'released' && <span className="badge badge-success">{t('Released')}</span>}
          {v.status === 'archived' && <span className="badge">{t('Archived')}</span>}
          {v.status === 'unreleased' && !overdue && <span className="badge badge-accent">{t('Unreleased')}</span>}
          {overdue && <span className="badge badge-danger"><TriangleExclamationPoint size={12} aria-hidden /> {t('Overdue')}</span>}
        </div>
        <span className="spacer" />
        {canEdit && v.status === 'unreleased' && (
          <button className="btn btn-sm" onClick={onRelease}><Rocket size={14} /> {t('Release [[action]]')}</button>
        )}
        {(canEdit || wiki) && (
          <PopButton className="icon-btn" title={t('More actions')} label={<Ellipsis size={16} />} width={250} align="end">
            {(close) => (
              <div className="rel-menu">
                {canEdit && (
                  <button className="menu-item" onClick={() => { close(); onEdit() }}><Pencil size={15} /> {t('Edit')}</button>
                )}
                {wiki && (
                  <button className="menu-item" disabled={busy} onClick={() => { close(); writeNotes() }}><BookOpen size={15} /> {t('Write release notes to the wiki')}</button>
                )}
                {canEdit && v.status === 'released' && (
                  <button className="menu-item" onClick={() => { close(); setStatus('unreleased', t('{name} is unreleased again', { name: v.name })) }}><Undo2 size={15} /> {t('Mark as unreleased')}</button>
                )}
                {canEdit && v.status !== 'archived' && (
                  <button className="menu-item" onClick={() => { close(); setStatus('archived', t('{name} archived', { name: v.name })) }}><Archive size={15} /> {t('Archive')}</button>
                )}
                {canEdit && v.status === 'archived' && (
                  <button className="menu-item" onClick={() => { close(); setStatus(v.releasedAt ? 'released' : 'unreleased', t('{name} restored', { name: v.name })) }}><ArchiveArrowUp size={15} /> {t('Restore from archive')}</button>
                )}
                {canEdit && (
                  <>
                    <div className="divider" />
                    <button className="menu-item danger" onClick={() => { close(); onDelete() }}><Trash size={15} /> {t('Delete')}</button>
                  </>
                )}
              </div>
            )}
          </PopButton>
        )}
      </div>

      <div className="rel-dates">
        <CalendarDays size={14} aria-hidden />
        {v.startDate && <span>{t('Start {date}', { date: shortDate(v.startDate) })}</span>}
        {v.status === 'unreleased' && v.releaseDate && (
          <span className={overdue ? 'rel-date-overdue' : ''}>
            {t('Release {date}', { date: shortDate(v.releaseDate) })}
            {days !== null && (
              <span className="rel-days">
                {' · '}
                {days < 0 ? t('{count} days overdue', { count: -days }) : days === 0 ? t('due today') : t('in {count} days', { count: days })}
              </span>
            )}
          </span>
        )}
        {v.status !== 'unreleased' && (v.releasedAt || v.releaseDate) && <span>{t('Released {date}', { date: shortDate(v.releasedAt ?? v.releaseDate) })}</span>}
        {!v.startDate && !v.releaseDate && !v.releasedAt && <span className="faint">{t('No dates')}</span>}
      </div>

      {v.description && <p className="rel-desc">{v.description}</p>}

      <div className="rel-progress">
        {issues.isLoading ? (
          <div className="progress" aria-hidden />
        ) : (
          <div
            className="progress rel-bar"
            role="img"
            aria-label={t('{done} of {total} issues done', { done: c.done, total: c.total })}
          >
            <span className="p-done" style={{ width: `${pct(c.done)}%` }} />
            <span className="p-progress" style={{ width: `${pct(c.progress)}%` }} />
          </div>
        )}
        <div className="rel-counts">
          {issues.error ? (
            <span className="error-text">{errMsg(issues.error)}</span>
          ) : c.total === 0 && !issues.isLoading ? (
            <span className="faint">{t('No issues in this version')}</span>
          ) : (
            <>
              <span className="rel-count"><b>{c.total}</b> {t('issues')}</span>
              <span className="rel-count"><i className="dot rel-dot-done" /> {t('{count} done', { count: c.done })}</span>
              <span className="rel-count"><i className="dot rel-dot-progress" /> {t('{count} in progress', { count: c.progress })}</span>
              <span className="rel-count"><i className="dot rel-dot-todo" /> {t('{count} to do', { count: c.todo })}</span>
              {c.total > 0 && <span className="rel-count rel-pct">{Math.round(pct(c.done))}%</span>}
            </>
          )}
        </div>
      </div>

      {open && (
        <div className="rel-issues" id={listId}>
          {issues.isLoading && <Loading />}
          {issues.data && sorted.length === 0 && <div className="rel-none">{t('No issues have this version yet. Set the version on an issue to add it.')}</div>}
          {sorted.map((i) => (
            <button key={i.id} className="rel-issue" onClick={() => openIssue(i.key)}>
              <TypeIcon icon={i.type.icon} color={i.type.color} title={i.type.name} />
              <span className="issue-key">{i.key}</span>
              <span className={`rel-issue-title ${i.status.category === 'done' ? 'rel-issue-done' : ''}`}>{i.title}</span>
              <Lozenge name={i.status.name} category={i.status.category} />
              {i.assignee ? <Avatar name={i.assignee.name} id={i.assignee.id} size={22} /> : <Unassigned size={22} />}
            </button>
          ))}
        </div>
      )}
    </article>
  )
}

function VersionDialog({ project, version, onClose }: { project: Project; version: Version | null; onClose: () => void }) {
  const [name, setName] = useState(version?.name ?? '')
  const [description, setDescription] = useState(version?.description ?? '')
  const [startDate, setStartDate] = useState(version?.startDate ?? '')
  const [releaseDate, setReleaseDate] = useState(version?.releaseDate ?? '')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const qc = useQueryClient()
  const toast = useToast()
  const taken = project.versions.some((v) => v.id !== version?.id && v.name.trim().toLowerCase() === name.trim().toLowerCase())
  const badDates = !!startDate && !!releaseDate && releaseDate < startDate

  const save = async () => {
    setBusy(true)
    setErr('')
    const body = { name: name.trim(), description: description.trim() || null, startDate: startDate || null, releaseDate: releaseDate || null }
    try {
      if (version) await api.patch(`/versions/${version.id}`, body)
      else await api.post(`/projects/${project.key}/versions`, body)
      qc.invalidateQueries({ queryKey: ['project', project.key] })
      if (version && version.name !== body.name) refreshIssues(qc)
      toast(version ? t('Version {name} saved', { name: body.name }) : t('Version {name} created', { name: body.name }))
      onClose()
    } catch (e) {
      setErr(errMsg(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      title={version ? t('Edit version {name}', { name: version.name }) : t('Create version')}
      onClose={onClose}
      width={520}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !name.trim() || badDates} onClick={save}>
            {version ? t('Save') : t('Create version')}
          </button>
        </>
      }
    >
      <form
        onSubmit={(e) => {
          e.preventDefault()
          if (!busy && name.trim() && !badDates) save()
        }}
      >
        <div className="field">
          <label htmlFor="rel-name">{t('Name')}</label>
          <input id="rel-name" className="input" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder={t('e.g. 2.6 or Spring release')} />
          {taken && <span className="field-hint rel-warn">{t('Another version already has this name.')}</span>}
        </div>
        <div className="field">
          <label htmlFor="rel-desc">{t('Description (optional)')}</label>
          <textarea id="rel-desc" className="textarea" rows={3} value={description} onChange={(e) => setDescription(e.target.value)} placeholder={t('What ships in this release?')} />
        </div>
        <div className="rel-date-fields">
          <div className="field">
            <label htmlFor="rel-start">{t('Start date')}</label>
            <input id="rel-start" type="date" className="input" value={startDate} onChange={(e) => setStartDate(e.target.value)} />
          </div>
          <div className="field">
            <label htmlFor="rel-release">{t('Release date')}</label>
            <input id="rel-release" type="date" className="input" value={releaseDate} min={startDate || undefined} onChange={(e) => setReleaseDate(e.target.value)} />
          </div>
        </div>
        {badDates && <p className="error-text">{t('The release date is before the start date.')}</p>}
        {err && <p className="error-text">{err}</p>}
        <button type="submit" hidden />
      </form>
    </Modal>
  )
}

function ReleaseDialog({ project, version, onClose }: { project: Project; version: Version; onClose: () => void }) {
  const issues = useVersionIssues(project.key, version)
  const c = counts(issues.data?.issues)
  const unfinished = c.total - c.done
  const others = project.versions.filter((v) => v.status === 'unreleased' && v.id !== version.id).sort((a, b) => (a.releaseDate ?? '9999').localeCompare(b.releaseDate ?? '9999'))
  const [moveTo, setMoveTo] = useState<string>(others[0]?.id ?? '')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const qc = useQueryClient()
  const toast = useToast()

  const release = async () => {
    setBusy(true)
    setErr('')
    try {
      const r = await api.post<{ moved: number }>(`/versions/${version.id}/release`, { moveTo: moveTo || null })
      qc.invalidateQueries({ queryKey: ['project', project.key] })
      qc.invalidateQueries({ queryKey: ['projects'] })
      refreshIssues(qc)
      toast(r.moved ? t('Released {name} · {count} issues moved', { name: version.name, count: r.moved }) : t('Released {name}', { name: version.name }))
      onClose()
    } catch (e) {
      setErr(errMsg(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      title={t('Release {name}', { name: version.name })}
      onClose={onClose}
      width={480}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || issues.isLoading} onClick={release}>
            <Rocket size={15} /> {t('Release [[action]]')}
          </button>
        </>
      }
    >
      {issues.isLoading ? (
        <Loading />
      ) : (
        <>
          <p className="rel-release-summary">
            {c.total === 0
              ? t('This version has no issues.')
              : unfinished === 0
                ? t('All {count} issues are done.', { count: c.total })
                : t('{done} of {total} issues are done.', { done: c.done, total: c.total })}
          </p>
          {unfinished > 0 && (
            <div className="field">
              <label htmlFor="rel-move">{t('Move the {count} unfinished issues to', { count: unfinished })}</label>
              <select id="rel-move" className="select" value={moveTo} onChange={(e) => setMoveTo(e.target.value)}>
                {others.map((v) => (
                  <option key={v.id} value={v.id}>
                    {v.releaseDate ? t('{name} (release {date})', { name: v.name, date: shortDate(v.releaseDate) }) : v.name}
                  </option>
                ))}
                <option value="">{t('No version (remove them from this release)')}</option>
              </select>
              {others.length === 0 && <span className="field-hint">{t('There’s no other unreleased version; create one first to move them there.')}</span>}
            </div>
          )}
          <p className="faint small">
            {version.releaseDate
              ? t('The release date stays {date}.', { date: shortDate(version.releaseDate) })
              : t('Today becomes the release date.')}
          </p>
        </>
      )}
      {err && <p className="error-text">{err}</p>}
    </Modal>
  )
}
