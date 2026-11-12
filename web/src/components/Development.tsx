// Development: the pull requests, branches and commits linked to an issue through the Git
// integration (by its key in commit messages, branch names and pull request titles), a branch
// name to start work with, and how their history entries read.
import { t } from '../i18n'
import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { Copy, GitBranch, GitCommitHorizontal, GitMerge, GitPullRequest, GitPullRequestClosed } from 'lucide-react'
import { api, ago, dateTime, type HistoryEvent, type IssueDetail } from '../api'
import { PopButton, useToast } from '../ui'
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
}

interface DevData {
  links: DevLink[]
  /** the project has a repository connected */
  connected: boolean
  branchName: string
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
export function PrIcon({ state, size = 15, withTitle }: { state: DevLink['state']; size?: number; withTitle?: boolean }) {
  const Icon = state === 'merged' ? GitMerge : state === 'closed' ? GitPullRequestClosed : GitPullRequest
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

export default function Development({ issue }: { issue: IssueDetail }) {
  const q = useQuery({ queryKey: ['issue', issue.key, 'development'], queryFn: () => api.get<DevData>(`/issues/${issue.key}/development`) })
  const [allCommits, setAllCommits] = useState(false)
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
        <BranchName name={d.branchName} />
      </div>
      {d.links.length === 0 ? (
        <div className="faint small">{t('Nothing linked yet. Mention {key} in a commit message, a branch name or a pull request title.', { key: issue.key })}</div>
      ) : (
        <div className="dev-list">
          {prs.map((l) => (
            <div key={l.id} className="dev-row">
              <PrIcon state={l.state} />
              <a href={l.url} target="_blank" rel="noreferrer noopener" className="dev-title">
                {l.number != null && <span className="issue-key">#{l.number}</span>} {l.title}
              </a>
              <Meta l={l} />
              <span className={`badge ${l.state === 'merged' ? 'badge-accent' : l.state === 'closed' ? 'badge-danger' : 'badge-success'}`}>{prStateLabel(l.state)}</span>
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
