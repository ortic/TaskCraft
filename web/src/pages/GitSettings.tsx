// Project settings → Integrations → Git: connected repositories (each with its webhook address;
// the secret is shown once, with the setup steps for its provider) and the rules that move
// linked issues when branches are pushed or pull requests opened and merged.
import { t } from '../i18n'
import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Copy, GitBranch, KeyRound, Plus, Server, Trash, TriangleAlert, Webhook } from 'lucide-react'
import { api, ago, type Project } from '../api'
import { Confirm, ErrorBox, Loading, Modal, useToast } from '../ui'
import '../components/Development.css'

type Provider = 'github' | 'gitlab' | 'generic'

interface Hook {
  id: string
  provider: Provider
  repoUrl: string
  repo: string
  webhookUrl: string
  createdAt: string
  deliveries: number
  lastEvent: string | null
  lastEventAt: string | null
  lastError: string | null
  /** only right after connecting or a new secret */
  secret?: string
}

interface GitData {
  integrations: Hook[]
  rules: { mergedStatusId: string | null; openedStatusId: string | null }
}

const PROVIDERS: { id: Provider; label: () => string; hint: () => string }[] = [
  { id: 'github', label: () => 'GitHub', hint: () => t('Also Gitea and Forgejo (they send the same events)') },
  { id: 'gitlab', label: () => 'GitLab', hint: () => t('gitlab.com or your own GitLab') },
  { id: 'generic', label: () => t('Other server'), hint: () => t('Any server or CI job that can send JSON') },
]
const providerLabel = (p: Provider) => PROVIDERS.find((x) => x.id === p)?.label() ?? p

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e))

function CopyText({ text, label, mono = true }: { text: string; label: string; mono?: boolean }) {
  const toast = useToast()
  return (
    <div className="git-hook-url">
      {mono ? <code title={text}>{text}</code> : <span>{text}</span>}
      <button
        type="button"
        className="icon-btn icon-btn-sm"
        title={t('Copy')}
        aria-label={label}
        onClick={() => void navigator.clipboard?.writeText(text).then(() => toast(t('Copied')), () => toast(t('Copying didn’t work: select the text instead')))}
      >
        <Copy size={13} />
      </button>
    </div>
  )
}

export default function GitSettings({ project }: { project: Project }) {
  const qc = useQueryClient()
  const toast = useToast()
  const qk = ['git-settings', project.key]
  const q = useQuery({ queryKey: qk, queryFn: () => api.get<GitData>(`/projects/${project.key}/git`) })
  const [connecting, setConnecting] = useState(false)
  const [shown, setShown] = useState<Hook | null>(null)
  const [removing, setRemoving] = useState<Hook | null>(null)
  const [rotating, setRotating] = useState<Hook | null>(null)
  const refresh = () => qc.invalidateQueries({ queryKey: qk })
  const saveRules = async (rules: GitData['rules']) => {
    qc.setQueryData<GitData>(qk, (old) => (old ? { ...old, rules } : old))
    try {
      await api.put(`/projects/${project.key}/git/rules`, rules)
      toast(t('Saved'))
    } catch (e) {
      toast(errText(e))
    }
    refresh()
  }
  const hooks = q.data?.integrations ?? []
  const rules = q.data?.rules ?? { mergedStatusId: null, openedStatusId: null }
  return (
    <section className="ps-section">
      <div className="ps-section-head">
        <div className="ps-section-text">
          <h2><span className="row"><GitBranch size={17} aria-hidden /> {t('Git')}</span></h2>
          <p className="ps-desc">
            {t('Connect repositories on GitHub, GitLab or another Git server: commits, branches and pull requests that mention an issue key (like {key}-12) show up on the issue.', { key: project.key })}
          </p>
        </div>
        <div className="ps-section-actions">
          <button className="btn btn-primary" onClick={() => setConnecting(true)}><Plus size={15} /> {t('Connect a repository')}</button>
        </div>
      </div>
      {q.isLoading && <Loading />}
      {q.error && <ErrorBox error={q.error} />}
      {q.data && hooks.length === 0 && (
        <div className="ps-note">
          <Webhook size={18} aria-hidden />
          <div>{t('No repository connected yet. Each repository gets its own webhook address and secret.')}</div>
        </div>
      )}
      <div className="ps-list">
        {hooks.map((h) => (
          <div key={h.id} className="ps-row">
            <span className="ps-kind-icon" title={providerLabel(h.provider)}>{h.provider === 'generic' ? <Server size={15} aria-hidden /> : <GitBranch size={15} aria-hidden />}</span>
            <div className="ps-row-main">
              <span>
                <a href={h.repoUrl} target="_blank" rel="noreferrer noopener" className="ps-strong">{h.repo || h.repoUrl}</a>
                <span className="faint small"> · {providerLabel(h.provider)}</span>
              </span>
              <CopyText text={h.webhookUrl} label={t('Copy the webhook address')} />
              <span className="faint small">
                {h.lastEventAt
                  ? t('Last delivery {when} ({event}), {count} in all', { when: ago(h.lastEventAt), event: h.lastEvent ?? '', count: h.deliveries })
                  : t('No deliveries yet')}
              </span>
              {h.lastError && <span className="error-text small"><TriangleAlert size={12} aria-hidden /> {h.lastError}</span>}
            </div>
            <button className="icon-btn" title={t('New secret')} aria-label={t('New secret for {repo}', { repo: h.repo })} onClick={() => setRotating(h)}><KeyRound size={15} /></button>
            <button className="icon-btn" title={t('Disconnect')} aria-label={t('Disconnect {repo}', { repo: h.repo })} onClick={() => setRemoving(h)}><Trash size={15} /></button>
          </div>
        ))}
      </div>
      {q.data && (
        <>
          <div className="git-rules">
            <div className="field">
              <label htmlFor="git-rule-open">{t('When a branch is pushed or a pull request opened, move its issues to')}</label>
              <select id="git-rule-open" className="select" value={rules.openedStatusId ?? ''} onChange={(e) => saveRules({ ...rules, openedStatusId: e.target.value || null })}>
                <option value="">{t('Don’t move them')}</option>
                {project.statuses.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
              </select>
            </div>
            <div className="field">
              <label htmlFor="git-rule-merge">{t('When a pull request is merged, move its issues to')}</label>
              <select id="git-rule-merge" className="select" value={rules.mergedStatusId ?? ''} onChange={(e) => saveRules({ ...rules, mergedStatusId: e.target.value || null })}>
                <option value="">{t('Don’t move them')}</option>
                {project.statuses.map((s) => <option key={s.id} value={s.id}>{s.name}</option>)}
              </select>
            </div>
          </div>
          <p className="field-hint ps-mt-0">
            {t('The moves are made by Automation, only forward in the workflow (never back from a later status) and, in a restricted workflow, only along allowed transitions.')}
          </p>
        </>
      )}
      {connecting && (
        <ConnectDialog
          project={project}
          onClose={() => setConnecting(false)}
          onCreated={(h) => {
            setConnecting(false)
            setShown(h)
            refresh()
          }}
        />
      )}
      {shown && <SecretDialog hook={shown} projectKey={project.key} onClose={() => setShown(null)} />}
      {rotating && (
        <Confirm
          title={t('New secret for {repo}?', { repo: rotating.repo })}
          body={<p>{t('The webhook address and the secret change: deliveries fail until you update them in the repository’s webhook settings.')}</p>}
          confirmLabel={t('Make a new secret')}
          onClose={() => setRotating(null)}
          onConfirm={async () => {
            try {
              const h = await api.post<Hook>(`/git-integrations/${rotating.id}/secret`)
              setRotating(null)
              setShown(h)
            } catch (e) {
              toast(errText(e))
            }
            refresh()
          }}
        />
      )}
      {removing && (
        <Confirm
          title={t('Disconnect {repo}?', { repo: removing.repo })}
          body={<p>{t('Deliveries from it are refused from now on. Links already on issues stay. Remove the webhook in the repository too.')}</p>}
          confirmLabel={t('Disconnect')}
          danger
          onClose={() => setRemoving(null)}
          onConfirm={async () => {
            try {
              await api.del(`/git-integrations/${removing.id}`)
              setRemoving(null)
              toast(t('Disconnected'))
            } catch (e) {
              toast(errText(e))
            }
            refresh()
          }}
        />
      )}
    </section>
  )
}

function ConnectDialog({ project, onClose, onCreated }: { project: Project; onClose: () => void; onCreated: (h: Hook) => void }) {
  const toast = useToast()
  const [provider, setProvider] = useState<Provider>('github')
  const [repoUrl, setRepoUrl] = useState('')
  const [busy, setBusy] = useState(false)
  const valid = /^https?:\/\/\S+$/.test(repoUrl.trim())
  return (
    <Modal
      title={t('Connect a repository to {name}', { name: project.name })}
      onClose={onClose}
      width={560}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button
            className="btn btn-primary"
            disabled={!valid || busy}
            onClick={async () => {
              setBusy(true)
              try {
                onCreated(await api.post<Hook>(`/projects/${project.key}/git`, { provider, repoUrl: repoUrl.trim() }))
              } catch (e) {
                toast(errText(e))
              } finally {
                setBusy(false)
              }
            }}
          >{t('Connect')}</button>
        </>
      }
    >
      <div className="field">
        <span className="field-label" id="git-provider">{t('Where the repository lives')}</span>
        <div className="ps-kind-grid" role="radiogroup" aria-labelledby="git-provider">
          {PROVIDERS.map((p) => (
            <button key={p.id} type="button" role="radio" aria-checked={provider === p.id} className={`ps-kind ${provider === p.id ? 'on' : ''}`} onClick={() => setProvider(p.id)} title={p.hint()}>
              {p.id === 'generic' ? <Server size={15} aria-hidden /> : <GitBranch size={15} aria-hidden />} {p.label()}
            </button>
          ))}
        </div>
        <span className="field-hint">{PROVIDERS.find((p) => p.id === provider)?.hint()}</span>
      </div>
      <div className="field">
        <label htmlFor="git-repo-url">{t('Repository address')}</label>
        <input
          id="git-repo-url"
          className="input"
          autoFocus
          value={repoUrl}
          placeholder={provider === 'gitlab' ? 'https://gitlab.com/acme/shop' : 'https://github.com/acme/shop'}
          onChange={(e) => setRepoUrl(e.target.value)}
        />
        <span className="field-hint">{t('Its web address: links to commits and branches are made from it.')}</span>
      </div>
      <p className="faint small">{t('Only issues of this project are linked (also under its former keys).')}</p>
    </Modal>
  )
}

function SecretDialog({ hook, projectKey, onClose }: { hook: Hook; projectKey: string; onClose: () => void }) {
  const secret = hook.secret ?? ''
  const example = `curl -X POST '${hook.webhookUrl}' -H 'Content-Type: application/json' -H 'X-TaskCraft-Token: ${secret}' \\\n  -d '{"commits": [{"sha": "3f2a9c1", "message": "${projectKey}-12 Fix the login timeout", "url": "${hook.repoUrl}/commit/3f2a9c1", "author": "Max"}]}'`
  return (
    <Modal title={t('Set up the webhook for {repo}', { repo: hook.repo })} onClose={onClose} width={640} footer={<button className="btn btn-primary" onClick={onClose}>{t('Close')}</button>}>
      <div className="git-secret">
        <div className="ps-note">
          <TriangleAlert size={18} aria-hidden />
          <div>{t('The secret is shown only now. Copy it into the webhook settings; if it gets lost, make a new one.')}</div>
        </div>
        <div className="field">
          <span className="field-label">{t('Webhook address')}</span>
          <CopyText text={hook.webhookUrl} label={t('Copy the webhook address')} />
        </div>
        <div className="field">
          <span className="field-label">{t('Secret')}</span>
          <CopyText text={secret} label={t('Copy the secret')} />
        </div>
        {hook.provider === 'github' && (
          <ol>
            <li>{t('In the repository on GitHub, open Settings → Webhooks → Add webhook.')}</li>
            <li>{t('Payload URL: the webhook address. Content type: application/json. Secret: the secret.')}</li>
            <li>{t('Under “Which events”, choose “Let me select individual events”: Pushes and Pull requests.')}</li>
            <li>{t('Add the webhook. GitHub sends a ping right away; it shows up here as the last delivery.')}</li>
          </ol>
        )}
        {hook.provider === 'gitlab' && (
          <ol>
            <li>{t('In the project on GitLab, open Settings → Webhooks → Add new webhook.')}</li>
            <li>{t('URL: the webhook address. Secret token: the secret.')}</li>
            <li>{t('Triggers: Push events and Merge request events.')}</li>
            <li>{t('Save, then send a test (Test → Push events): it shows up here as the last delivery.')}</li>
          </ol>
        )}
        {hook.provider === 'generic' && (
          <>
            <p className="ps-mt-0">{t('Send a POST with JSON to the webhook address, with the secret in the X-TaskCraft-Token header (or an HMAC-SHA256 signature of the body in X-Hub-Signature-256: sha256=…). Fields: commits, branch and pullRequest, all optional (see the README).')}</p>
            <pre className="ps-mono small" style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere', margin: 0 }}>{example}</pre>
          </>
        )}
      </div>
    </Modal>
  )
}
