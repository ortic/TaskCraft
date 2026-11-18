// Development: the pull requests, branches and commits linked to an issue through the Git
// integration (by its key in commit messages, branch names and pull request titles), a branch
// name to start work with, and how their history entries read.
import { t } from '../i18n'
import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { CircleCheck, CircleDashed, CircleX, Copy, GitBranch, GitBranchPlus, GitCommitHorizontal, GitMerge, GitPullRequest, GitPullRequestClosed, GitPullRequestDraft } from 'lucide-react'
import { api, ago, dateTime, type HistoryEvent, type IssueDetail } from '../api'
import { Modal, PopButton, useToast } from '../ui'
import './Development.css'

export interface DevLink {
  id: string
  kind: 'commit' | 'branch' | 'pull_request'
  title: string
  url: string
  author: string | null
  state: 'open' | 'merged' | 'closed' | null
  sha: string | null
  branch: string | null
  number: number | null
  repo: string | null
  at: string
  /** pull requests: not ready for review yet */
  draft?: boolean | null
  /** pull requests: checks / CI as the Git server last reported */
  checks?: 'pending' | 'success' | 'failure' | null
  review?: 'approved' | 'changes_requested' | null
  reviewers?: { name: string; state: 'requested' | 'approved' | 'changes_requested' }[] | null
}

/** A repository a branch can be created in. */
interface DevRepo {
  id: string
  repo: string
  kind: 'github' | 'gitlab' | 'gitea'
  defaultBranch: string | null
}

interface DevData {
  links: DevLink[]
  /** the project has a repository connected */
  connected: boolean
  branchName: string
  /** where the caller may create a branch for the issue */
  repositories?: DevRepo[]
}

const COMMITS_SHOWN = 5

function copy(text: string, done: () => void) {
  void navigator.clipboard?.writeText(text).then(done, done)
}

/** "web-12-short-title" to copy, and the git command that starts the branch. */
export function BranchName({ name }: { name: string }) {
  const toast = useToast()
  return (
    <PopButton className="btn btn-sm" label={<><GitBranch size={14} /> {t('Branch name')}</>} align="end" width={340}>
      {(close) => (
        <div className="dev-branch-pop">
          <div className="faint small">{t('Start the work on a branch named after the issue: its commits and pull requests are linked here.')}</div>
          {[name, `git checkout -b ${name}`].map((text) => (
            <button
              key={text}
              className="dev-copy"
              title={t('Copy')}
              onClick={() =>
                copy(text, () => {
                  toast(t('Copied'))
                  close()
                })
              }
            >
              <code>{text}</code>
              <Copy size={13} aria-hidden />
            </button>
          ))}
        </div>
      )}
    </PopButton>
  )
}

/** A pull request's state as an icon (green open, merged, red closed); with a title on issue cards. */
export function PrIcon({ state, size = 15, withTitle, draft }: { state: DevLink['state']; size?: number; withTitle?: boolean; draft?: boolean }) {
  const Icon = state === 'merged' ? GitMerge : state === 'closed' ? GitPullRequestClosed : draft ? GitPullRequestDraft : GitPullRequest
  const cls = state === 'merged' ? 'dev-merged' : state === 'closed' ? 'dev-closed' : 'dev-open'
  if (!withTitle) return <Icon size={size} className={cls} aria-hidden />
  const label = t('Pull request: {state}', { state: prStateLabel(state) })
  return (
    <span className="ic-sub" title={label} aria-label={label} role="img">
      <Icon size={size} className={cls} aria-hidden />
    </span>
  )
}

export function prStateLabel(state: DevLink['state']): string {
  return state === 'merged' ? t('merged') : state === 'closed' ? t('closed [[pull request]]') : t('open [[pull request]]')
}

function Meta({ l }: { l: DevLink }) {
  const parts = [l.repo, l.author].filter(Boolean)
  return (
    <span className="faint small dev-meta" title={dateTime(l.at)}>
      {parts.join(' · ')}{parts.length > 0 && ' · '}{ago(l.at)}
    </span>
  )
}

/** Checks / CI of a pull request, as the Git server last reported. */
function Checks({ checks }: { checks: NonNullable<DevLink['checks']> }) {
  const label = checks === 'success' ? t('Checks passed') : checks === 'failure' ? t('Checks failed') : t('Checks running')
  const Icon = checks === 'success' ? CircleCheck : checks === 'failure' ? CircleX : CircleDashed
  return (
    <span className={`dev-checks dev-checks-${checks}`} title={label} aria-label={label} role="img">
      <Icon size={14} aria-hidden />
    </span>
  )
}

/** Where a pull request's reviews stand, with who was asked and what they said. */
function Reviews({ l }: { l: DevLink }) {
  const list = l.reviewers ?? []
  if (!l.review && list.length === 0) return null
  const word = (s: string) => (s === 'approved' ? t('approved [[review]]') : s === 'changes_requested' ? t('asks for changes') : t('asked'))
  const title = list.map((r) => `${r.name}: ${word(r.state)}`).join('\n')
  const waiting = list.filter((r) => r.state === 'requested').length
  if (l.review === 'changes_requested') return <span className="badge badge-danger" title={title}>{t('Changes requested')}</span>
  if (l.review === 'approved') return <span className="badge badge-success" title={title}>{t('Approved [[review]]')}</span>
  return <span className="badge" title={title}>{t('{count} reviewers asked', { count: waiting })}</span>
}

/** Creates the issue's branch on the Git server, optionally with a draft pull request. */
function CreateBranch({ issue, d, onClose }: { issue: IssueDetail; d: DevData; onClose: () => void }) {
  const qc = useQueryClient()
  const toast = useToast()
  const repos = d.repositories ?? []
  const [repoId, setRepoId] = useState(repos[0]?.id ?? '')
  const repo = repos.find((r) => r.id === repoId) ?? repos[0]
  const [name, setName] = useState(d.branchName)
  const [base, setBase] = useState('')
  const [pr, setPr] = useState(false)
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const create = async () => {
    setBusy(true)
    setErr('')
    try {
      const r = await api.post<{ branch: { name: string }; pullRequest?: { url: string }; warning?: string }>(`/issues/${issue.key}/branch`, {
        integrationId: repo.id,
        name: name.trim(),
        base: base.trim() || undefined,
        pullRequest: pr,
      })
      toast(r.warning ?? (r.pullRequest ? t('Branch {name} and a draft pull request created', { name: r.branch.name }) : t('Branch {name} created', { name: r.branch.name })))
      qc.invalidateQueries({ queryKey: ['issue', issue.key] })
      qc.invalidateQueries({ queryKey: ['history', issue.key] })
      onClose()
    } catch (e) {
      setErr((e as Error).message)
      setBusy(false)
    }
  }
  return (
    <Modal
      title={t('Create a branch for {key}', { key: issue.key })}
      onClose={onClose}
      width={520}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !name.trim()} onClick={create}><GitBranchPlus size={15} /> {t('Create branch')}</button>
        </>
      }
    >
      {repos.length > 1 && (
        <div className="field">
          <label htmlFor="dev-repo">{t('Repository')}</label>
          <select id="dev-repo" className="select" value={repo.id} onChange={(e) => setRepoId(e.target.value)}>
            {repos.map((r) => <option key={r.id} value={r.id}>{r.repo}</option>)}
          </select>
        </div>
      )}
      <div className="field">
        <label htmlFor="dev-branch">{t('Branch name')}</label>
        <input id="dev-branch" className="input dev-mono" value={name} maxLength={200} spellCheck={false} onChange={(e) => setName(e.target.value)} />
        {repos.length === 1 && <span className="field-hint">{t('In {repo}', { repo: repo.repo })}</span>}
      </div>
      <div className="field">
        <label htmlFor="dev-base">{t('From')}</label>
        <input id="dev-base" className="input dev-mono" value={base} maxLength={200} spellCheck={false} placeholder={repo.defaultBranch ?? 'main'} onChange={(e) => setBase(e.target.value)} />
        <span className="field-hint">{t('Empty: the default branch ({branch})', { branch: repo.defaultBranch ?? 'main' })}</span>
      </div>
      <label className="row small dev-check">
        <input type="checkbox" checked={pr} onChange={(e) => setPr(e.target.checked)} /> {t('Also open a draft pull request')}
      </label>
      {pr && repo.kind === 'github' && <p className="field-hint">{t('GitHub opens a pull request only for a branch with a commit: an empty first commit is added.')}</p>}
      {err && <p className="error-text" role="alert">{err}</p>}
    </Modal>
  )
}

export default function Development({ issue }: { issue: IssueDetail }) {
  const q = useQuery({ queryKey: ['issue', issue.key, 'development'], queryFn: () => api.get<DevData>(`/issues/${issue.key}/development`) })
  const [allCommits, setAllCommits] = useState(false)
  const [creating, setCreating] = useState(false)
  const d = q.data
  if (!d || (!d.connected && d.links.length === 0)) return null
  const prs = d.links.filter((l) => l.kind === 'pull_request')
  const branches = d.links.filter((l) => l.kind === 'branch')
  const commits = d.links.filter((l) => l.kind === 'commit')
  const shown = allCommits ? commits : commits.slice(0, COMMITS_SHOWN)
  return (
    <div className="section">
      <div className="section-head">
        <h3>{t('Development')}</h3>
        {prs.length > 0 && <span className="badge">{t('{count} pull requests', { count: prs.length })}</span>}
        <span className="spacer" />
        {(d.repositories?.length ?? 0) > 0 && issue.permissions.edit && (
          <button className="btn btn-sm" onClick={() => setCreating(true)}><GitBranchPlus size={14} /> {t('Create branch')}</button>
        )}
        <BranchName name={d.branchName} />
      </div>
      {d.links.length === 0 ? (
        <div className="faint small">{t('Nothing linked yet. Mention {key} in a commit message, a branch name or a pull request title.', { key: issue.key })}</div>
      ) : (
        <div className="dev-list">
          {prs.map((l) => (
            <div key={l.id} className="dev-row">
              <PrIcon state={l.state} draft={!!l.draft} />
              <a href={l.url} target="_blank" rel="noreferrer noopener" className="dev-title">
                {l.number != null && <span className="issue-key">#{l.number}</span>} {l.title}
              </a>
              <Meta l={l} />
              {l.state === 'open' && l.checks && <Checks checks={l.checks} />}
              {l.state === 'open' && <Reviews l={l} />}
              <span className={`badge ${l.state === 'merged' ? 'badge-accent' : l.state === 'closed' ? 'badge-danger' : l.draft ? '' : 'badge-success'}`}>{l.state === 'open' && l.draft ? t('draft [[pull request]]') : prStateLabel(l.state)}</span>
            </div>
          ))}
          {branches.map((l) => (
            <div key={l.id} className="dev-row">
              <GitBranch size={15} className="faint" aria-hidden />
              <a href={l.url} target="_blank" rel="noreferrer noopener" className="dev-title dev-mono">{l.title}</a>
              <Meta l={l} />
            </div>
          ))}
          {shown.map((l) => (
            <div key={l.id} className="dev-row">
              <GitCommitHorizontal size={15} className="faint" aria-hidden />
              <a href={l.url} target="_blank" rel="noreferrer noopener" className="dev-title">
                <span className="issue-key">{(l.sha ?? '').slice(0, 7)}</span> {l.title}
              </a>
              <Meta l={l} />
            </div>
          ))}
          {commits.length > COMMITS_SHOWN && (
            <button className="btn btn-ghost btn-sm dev-more" onClick={() => setAllCommits(!allCommits)}>
              {allCommits ? t('Show fewer commits') : t('Show all {count} commits', { count: commits.length })}
            </button>
          )}
        </div>
      )}
      {creating && <CreateBranch issue={issue} d={d} onClose={() => setCreating(false)} />}
    </div>
  )
}

/** A Git history entry ("linked a commit", "pull request merged") as a sentence. */
export function describeDevEvent(e: HistoryEvent): React.ReactNode {
  const n = e.new ?? {}
  const link = (text: string) => (
    <a href={n.url} target="_blank" rel="noreferrer noopener">{text}</a>
  )
  const by = n.author ? <span className="faint"> ({n.author})</span> : null
  switch (e.field) {
    case 'commit':
      return n.count > 1 ? (
        <>{t('linked {count} commits', { count: n.count })}, {t('the first:')} {link(n.title)}{by}</>
      ) : (
        <>{t('linked a commit')} {link(n.title)}{by}</>
      )
    case 'branch':
      return <>{t('linked a branch')} {link(n.title)}{by}</>
    case 'pull_request':
      if (e.old && n.state === 'merged') return <>{t('noted that the pull request was merged:')} {link(n.title)}</>
      if (e.old && n.state === 'closed') return <>{t('noted that the pull request was closed:')} {link(n.title)}</>
      if (e.old) return <>{t('noted that the pull request was reopened:')} {link(n.title)}</>
      return <>{t('linked a pull request')} {link(n.title)} <span className="faint">({prStateLabel(n.state ?? 'open')})</span>{by}</>
    default:
      return t('linked development work')
  }
}
