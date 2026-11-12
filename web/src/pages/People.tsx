import { t } from '../i18n'
import { useMemo, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Check, ListFilter, Mail, Pencil, Plus, Search, Trash, Users } from 'lucide-react'
import { api, qs, type Issue, type Person } from '../api'
import { Avatar, Confirm, Empty, ErrorBox, Loading, Lozenge, Modal, Tabs, useMe, usePeople, useToast } from '../ui'
import { PROJECT_COLORS, PriorityIcon, TypeIcon } from '../icons'
import { useOpenIssue } from '../components/Layout'
import './People.css'

// ---- teams (Admin uses these too) ----

export interface Team {
  id: string
  name: string
  description: string | null
  color: string | null
  memberIds: string[]
}

export function useTeams() {
  return useQuery({ queryKey: ['teams'], queryFn: () => api.get<Team[]>('/teams'), staleTime: 60_000 })
}

interface PersonDetail extends Person {
  status: string
  teams: { id: string; name: string }[]
}

const roleLabel = (role?: string) =>
  role === 'owner' ? t('Owner') : role === 'admin' ? t('Admin') : role === 'guest' ? t('Guest') : role === 'member' ? t('Member') : ''

/** Creates or edits a team: name, description, colour and members. */
export function TeamDialog({ team, onClose }: { team?: Team; onClose: () => void }) {
  const me = useMe().data
  const people = usePeople()
  const qc = useQueryClient()
  const toast = useToast()
  const [name, setName] = useState(team?.name ?? '')
  const [description, setDescription] = useState(team?.description ?? '')
  const [color, setColor] = useState(team?.color ?? PROJECT_COLORS[0])
  const [members, setMembers] = useState<string[]>(team?.memberIds ?? (me?.user ? [me.user.id] : []))
  const [q, setQ] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const list = (people.data ?? []).filter((p) => !q || `${p.name} ${p.email ?? ''} ${p.title ?? ''}`.toLowerCase().includes(q.trim().toLowerCase()))
  const toggle = (id: string) => setMembers((m) => (m.includes(id) ? m.filter((x) => x !== id) : [...m, id]))
  const save = async () => {
    if (!name.trim()) return setErr(t('A team needs a name.'))
    setBusy(true)
    setErr('')
    try {
      const body = { name: name.trim(), description: description.trim(), color, memberIds: members }
      if (team) await api.patch(`/teams/${team.id}`, body)
      else await api.post('/teams', body)
      qc.invalidateQueries({ queryKey: ['teams'] })
      qc.invalidateQueries({ queryKey: ['person'] })
      toast(team ? t('Team {name} saved', { name: name.trim() }) : t('Team {name} created', { name: name.trim() }))
      onClose()
    } catch (e) {
      setErr((e as Error).message)
    } finally {
      setBusy(false)
    }
  }
  return (
    <Modal
      title={team ? t('Edit team') : t('New team')}
      onClose={onClose}
      width={560}
      footer={
        <>
          {err && <span className="error-text ppl-foot-err">{err}</span>}
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !name.trim()} onClick={save}>{team ? t('Save team') : t('Create team')}</button>
        </>
      }
    >
      <div className="field">
        <label htmlFor="ppl-team-name">{t('Name')}</label>
        <input id="ppl-team-name" className="input" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder={t('e.g. Mobile squad')} maxLength={100} />
      </div>
      <div className="field">
        <label htmlFor="ppl-team-desc">{t('Description (optional)')}</label>
        <input id="ppl-team-desc" className="input" value={description} onChange={(e) => setDescription(e.target.value)} placeholder={t('What the team works on')} />
      </div>
      <div className="field">
        <span className="field-label">{t('Colour')}</span>
        <div className="swatches">
          {PROJECT_COLORS.map((c) => (
            <button key={c} type="button" className={`swatch ${c === color ? 'on' : ''}`} style={{ background: c }} aria-label={c} aria-pressed={c === color} onClick={() => setColor(c)} />
          ))}
        </div>
      </div>
      <div className="field">
        <span className="field-label">{t('Members ({count})', { count: members.length })}</span>
        <input className="input input-sm" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t('Find people…')} aria-label={t('Find people')} />
        <div className="ppl-member-list" role="group" aria-label={t('Members')}>
          {people.isLoading && <Loading />}
          {list.map((p) => {
            const on = members.includes(p.id)
            return (
              <label key={p.id} className={`ppl-member ${on ? 'on' : ''}`}>
                <input type="checkbox" checked={on} onChange={() => toggle(p.id)} />
                <Avatar name={p.name} id={p.id} size={24} />
                <span className="ppl-member-name">{p.name}</span>
                {p.title && <span className="faint small ppl-member-title">{p.title}</span>}
              </label>
            )
          })}
          {people.data && list.length === 0 && <div className="faint small ppl-member-empty">{t('Nobody matches “{q}”', { q })}</div>}
        </div>
        <span className="field-hint">{t('Teams can be given access to projects as a whole.')}</span>
      </div>
    </Modal>
  )
}

export function DeleteTeam({ team, onClose }: { team: Team; onClose: () => void }) {
  const qc = useQueryClient()
  const toast = useToast()
  return (
    <Confirm
      title={t('Delete team {name}?', { name: team.name })}
      body={<p>{t('The people stay; projects that gave this team access lose that grant.')}</p>}
      confirmLabel={t('Delete team')}
      danger
      onClose={onClose}
      onConfirm={async () => {
        try {
          await api.del(`/teams/${team.id}`)
          qc.invalidateQueries({ queryKey: ['teams'] })
          qc.invalidateQueries({ queryKey: ['person'] })
          toast(t('Team {name} deleted', { name: team.name }))
        } catch (e) {
          toast((e as Error).message)
        }
      }}
    />
  )
}

export function TeamMembers({ team, people, max = 8, onPick }: { team: Team; people: Person[]; max?: number; onPick?: (p: Person) => void }) {
  const byId = new Map(people.map((p) => [p.id, p]))
  const members = team.memberIds.map((id) => byId.get(id)).filter((p): p is Person => !!p)
  if (members.length === 0) return <span className="faint small">{t('No members yet')}</span>
  return (
    <div className="ppl-stack">
      {members.slice(0, max).map((p) =>
        onPick ? (
          <button key={p.id} type="button" className="ppl-stack-item" onClick={(e) => { e.stopPropagation(); onPick(p) }} title={p.name} aria-label={p.name}>
            <Avatar name={p.name} id={p.id} size={26} />
          </button>
        ) : (
          <span key={p.id} className="ppl-stack-item"><Avatar name={p.name} id={p.id} size={26} /></span>
        ),
      )}
      {members.length > max && <span className="ppl-stack-more">+{members.length - max}</span>}
    </div>
  )
}

// ---- the directory ----

export default function People() {
  const me = useMe().data
  const people = usePeople()
  const teams = useTeams()
  const [tab, setTab] = useState<'people' | 'teams'>('people')
  const [q, setQ] = useState('')
  const [teamFilter, setTeamFilter] = useState<string | null>(null)
  const [person, setPerson] = useState<Person | null>(null)
  const [editing, setEditing] = useState<Team | 'new' | null>(null)
  const [deleting, setDeleting] = useState<Team | null>(null)
  const role = me?.user?.role
  const canEditTeams = !!role && role !== 'guest'
  const isAdmin = role === 'owner' || role === 'admin'
  const needle = q.trim().toLowerCase()

  const teamsOf = useMemo(() => {
    const m = new Map<string, Team[]>()
    for (const tm of teams.data ?? []) for (const id of tm.memberIds) m.set(id, [...(m.get(id) ?? []), tm])
    return m
  }, [teams.data])
  const filterTeam = (teams.data ?? []).find((tm) => tm.id === teamFilter) ?? null
  const shownPeople = (people.data ?? []).filter(
    (p) => (!filterTeam || filterTeam.memberIds.includes(p.id)) && (!needle || `${p.name} ${p.email ?? ''} ${p.title ?? ''}`.toLowerCase().includes(needle)),
  )
  const shownTeams = (teams.data ?? []).filter((tm) => !needle || `${tm.name} ${tm.description ?? ''}`.toLowerCase().includes(needle))

  return (
    <div className="page ppl-page">
      <div className="page-head">
        <h1>{t('People & teams')}</h1>
        <span className="spacer" />
        <div className="ppl-search">
          <Search size={15} aria-hidden />
          <input className="input" value={q} onChange={(e) => setQ(e.target.value)} placeholder={tab === 'people' ? t('Find people…') : t('Find teams…')} aria-label={tab === 'people' ? t('Find people') : t('Find teams')} />
        </div>
        {canEditTeams && <button className="btn btn-primary" onClick={() => setEditing('new')}><Plus size={15} /> {t('New team')}</button>}
      </div>

      <Tabs
        value={tab}
        onChange={setTab}
        tabs={[
          { id: 'people', label: <>{t('People')} <span className="ppl-count">{people.data?.length ?? ''}</span></> },
          { id: 'teams', label: <>{t('Teams')} <span className="ppl-count">{teams.data?.length ?? ''}</span></> },
        ]}
      />

      {tab === 'people' && (
        <>
          {(teams.data?.length ?? 0) > 0 && (
            <div className="ppl-filters" role="group" aria-label={t('Filter by team')}>
              <button type="button" className={`chip ${!teamFilter ? 'on' : ''}`} aria-pressed={!teamFilter} onClick={() => setTeamFilter(null)}>{t('Everyone')}</button>
              {teams.data!.map((tm) => (
                <button key={tm.id} type="button" className={`chip ${teamFilter === tm.id ? 'on' : ''}`} aria-pressed={teamFilter === tm.id} onClick={() => setTeamFilter(teamFilter === tm.id ? null : tm.id)}>
                  <span className="dot" style={{ background: tm.color ?? 'var(--text-faint)' }} /> {tm.name}
                </button>
              ))}
            </div>
          )}
          {people.isLoading && <Loading />}
          {people.error && <ErrorBox error={people.error} />}
          {people.data && shownPeople.length === 0 && (
            <Empty icon={<Users size={32} />} title={needle ? t('Nobody matches “{q}”', { q }) : t('Nobody here yet')}>
              {filterTeam && t('Try “Everyone” instead of one team.')}
            </Empty>
          )}
          <div className="ppl-grid">
            {shownPeople.map((p) => (
              <button key={p.id} type="button" className="ppl-card" onClick={() => setPerson(p)}>
                <Avatar name={p.name} id={p.id} size={42} />
                <span className="ppl-card-body">
                  <span className="ppl-card-name">
                    {p.name}
                    {p.id === me?.user?.id && <span className="badge">{t('You')}</span>}
                    {p.role && p.role !== 'member' && <span className={`badge ${p.role === 'guest' ? '' : 'badge-accent'}`}>{roleLabel(p.role)}</span>}
                  </span>
                  <span className="ppl-card-title">{p.title || <span className="faint">{t('No job title')}</span>}</span>
                  {p.email && <span className="ppl-card-email faint small">{p.email}</span>}
                  {(teamsOf.get(p.id)?.length ?? 0) > 0 && (
                    <span className="ppl-card-teams">
                      {teamsOf.get(p.id)!.map((tm) => (
                        <span key={tm.id} className="ppl-team-tag"><span className="dot" style={{ background: tm.color ?? 'var(--text-faint)' }} />{tm.name}</span>
                      ))}
                    </span>
                  )}
                </span>
              </button>
            ))}
          </div>
        </>
      )}

      {tab === 'teams' && (
        <>
          {teams.isLoading && <Loading />}
          {teams.error && <ErrorBox error={teams.error} />}
          {teams.data && shownTeams.length === 0 && (
            <Empty icon={<Users size={32} />} title={needle ? t('No team matches “{q}”', { q }) : t('No teams yet')}>
              {!needle && t('Teams group people so projects can give them access together.')}
            </Empty>
          )}
          <div className="ppl-team-grid">
            {shownTeams.map((tm) => (
              <div key={tm.id} className="ppl-team" style={{ '--team': tm.color ?? 'var(--border-strong)' } as React.CSSProperties}>
                <div className="ppl-team-head">
                  <span className="ppl-team-swatch" aria-hidden />
                  <div className="ppl-team-title">
                    <h2>{tm.name}</h2>
                    <span className="faint small">{t('{count} members', { count: tm.memberIds.length })}</span>
                  </div>
                  {canEditTeams && (
                    <button className="icon-btn" title={t('Edit team')} aria-label={t('Edit team')} onClick={() => setEditing(tm)}><Pencil size={15} /></button>
                  )}
                  {isAdmin && (
                    <button className="icon-btn" title={t('Delete team')} aria-label={t('Delete team')} onClick={() => setDeleting(tm)}><Trash size={15} /></button>
                  )}
                </div>
                {tm.description && <p className="ppl-team-desc small muted">{tm.description}</p>}
                <TeamMembers team={tm} people={people.data ?? []} onPick={setPerson} />
                <button
                  type="button"
                  className="btn btn-sm btn-ghost ppl-team-show"
                  onClick={() => {
                    setTeamFilter(tm.id)
                    setQ('')
                    setTab('people')
                  }}
                >
                  <ListFilter size={14} /> {t('Show members')}
                </button>
              </div>
            ))}
          </div>
        </>
      )}

      {person && <PersonDialog person={person} teams={teams.data ?? []} onClose={() => setPerson(null)} />}
      {editing && <TeamDialog team={editing === 'new' ? undefined : editing} onClose={() => setEditing(null)} />}
      {deleting && <DeleteTeam team={deleting} onClose={() => setDeleting(null)} />}
    </div>
  )
}

function PersonDialog({ person, teams, onClose }: { person: Person; teams: Team[]; onClose: () => void }) {
  const me = useMe().data
  const open = useOpenIssue()
  const nav = useNavigate()
  const detail = useQuery({ queryKey: ['person', person.id], queryFn: () => api.get<PersonDetail>(`/people/${person.id}`) })
  const tql = `assignee = "${person.id}" and category != done`
  // 'search' prefix: refreshed whenever issues change
  const work = useQuery({ queryKey: ['search', 'person', person.id], queryFn: () => api.get<{ total: number; issues: Issue[] }>(`/search${qs({ q: tql, limit: 50 })}`) })
  const p = detail.data ?? person
  const status = detail.data?.status
  const isMe = me?.user?.id === person.id
  const colorOf = (id: string) => teams.find((tm) => tm.id === id)?.color ?? 'var(--text-faint)'
  const issues = work.data?.issues ?? []
  const inProgress = issues.filter((i) => i.status.category === 'in_progress')
  const todo = issues.filter((i) => i.status.category !== 'in_progress')
  const row = (i: Issue) => (
    <button
      key={i.id}
      type="button"
      className="ppl-issue"
      onClick={() => {
        onClose()
        open(i.key)
      }}
    >
      <TypeIcon icon={i.type.icon} color={i.type.color} title={i.type.name} />
      <span className="issue-key">{i.key}</span>
      <span className="ppl-issue-title">{i.title}</span>
      <PriorityIcon priority={i.priority} />
      <Lozenge name={i.status.name} category={i.status.category} />
    </button>
  )
  return (
    <Modal title={t('Profile')} onClose={onClose} width={640} className="ppl-dialog">
      <div className="ppl-profile">
        <Avatar name={p.name} id={p.id} size={56} />
        <div className="ppl-profile-text">
          <div className="ppl-profile-name">
            {p.name}
            {p.role && p.role !== 'member' && <span className={`badge ${p.role === 'guest' ? '' : 'badge-accent'}`}>{roleLabel(p.role)}</span>}
            {status && status !== 'active' && <span className="badge badge-warning">{status === 'invited' ? t('Invited') : t('Deactivated')}</span>}
          </div>
          {p.title && <div className="muted">{p.title}</div>}
          {p.email && (
            <a className="small ppl-mail" href={`mailto:${p.email}`}><Mail size={13} /> {p.email}</a>
          )}
        </div>
        {isMe && <Link to="/settings" className="btn btn-sm" onClick={onClose}><Pencil size={13} /> {t('Edit profile')}</Link>}
      </div>

      <div className="ppl-section">
        <h3>{t('Teams')}</h3>
        {detail.isLoading && <span className="faint small">{t('Loading…')}</span>}
        {detail.data && detail.data.teams.length === 0 && <span className="faint small">{t('Not in any team')}</span>}
        <div className="ppl-card-teams">
          {(detail.data?.teams ?? []).map((tm) => (
            <span key={tm.id} className="ppl-team-tag"><span className="dot" style={{ background: colorOf(tm.id) }} />{tm.name}</span>
          ))}
        </div>
      </div>

      <div className="ppl-section">
        <div className="ppl-section-head">
          <h3>{t('Open issues assigned')}{work.data && <span className="ppl-count">{work.data.total}</span>}</h3>
          <span className="spacer" />
          {work.data && work.data.total > 0 && (
            <button
              type="button"
              className="btn btn-sm btn-ghost"
              onClick={() => {
                onClose()
                nav(`/search${qs({ q: tql })}`)
              }}
            >
              {t('Open as filter')}
            </button>
          )}
        </div>
        {work.isLoading && <Loading />}
        {work.error && <ErrorBox error={work.error} />}
        {work.data && issues.length === 0 && (
          <div className="ppl-none"><Check size={16} /> {isMe ? t('Nothing open is assigned to you.') : t('Nothing open is assigned to {name}.', { name: p.name.split(' ')[0] })}</div>
        )}
        {inProgress.length > 0 && <div className="menu-head">{t('In progress')}</div>}
        <div className="ppl-issues">{inProgress.map(row)}</div>
        {todo.length > 0 && inProgress.length > 0 && <div className="menu-head">{t('Up next')}</div>}
        <div className="ppl-issues">{todo.map(row)}</div>
        {work.data && work.data.total > issues.length && <p className="faint small">{t('Showing {shown} of {total}.', { shown: issues.length, total: work.data.total })}</p>}
      </div>
    </Modal>
  )
}
