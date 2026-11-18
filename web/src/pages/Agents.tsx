// Administration → Agents: AI coding agents (their name, handle, instructions, repository and
// whether they're being tried out), the runners that do their work, and what each agent did.
import { t } from '../i18n'
import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Bot, Copy, Pencil, Plus, ScrollText, Server, Trash, TriangleAlert } from 'lucide-react'
import { api, ago, dateTime } from '../api'
import { Confirm, Empty, ErrorBox, Loading, Modal, useToast } from '../ui'
import { useOpenIssue } from '../components/Layout'
import { runStateBadge, runStateLabel, type AgentRun } from '../components/AgentRuns'

interface Agent {
  id: string
  userId: string
  name: string
  handle: string
  instructions: string
  model: string | null
  integrationId: string | null
  baseBranch: string | null
  state: 'testing' | 'active' | 'off'
  repository: string | null
  runs: number
  activeRuns: number
  tokensIn: number
  tokensOut: number
}

interface Runner {
  id: string
  name: string
  registeredAt: string | null
  lastSeenAt: string | null
}

interface AgentsPage {
  agents: Agent[]
  runners: Runner[]
  repositories: { id: string; repo: string; projectKey: string; defaultBranch: string | null }[]
  modelConfigured: boolean
  serverUrl: string
}

const stateLabel = (s: Agent['state']) => (s === 'active' ? t('For everyone') : s === 'testing' ? t('Being tried out') : t('Off'))
const tokens = (n: number) => (n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${Math.round(n / 1000)}k` : String(n))

export default function AgentsTab() {
  const qc = useQueryClient()
  const toast = useToast()
  const q = useQuery({ queryKey: ['admin', 'agents'], queryFn: () => api.get<AgentsPage>('/admin/agents') })
  const [editing, setEditing] = useState<Agent | 'new' | null>(null)
  const [runsOf, setRunsOf] = useState<Agent | null>(null)
  const [removing, setRemoving] = useState<Agent | null>(null)
  const [addingRunner, setAddingRunner] = useState(false)
  const [removingRunner, setRemovingRunner] = useState<Runner | null>(null)
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['admin', 'agents'] })
    qc.invalidateQueries({ queryKey: ['people'] })
  }
  const d = q.data
  const del = async (path: string) => {
    try {
      await api.del(path)
    } catch (e) {
      toast((e as Error).message)
    }
    refresh()
  }
  return (
    <>
      <div className="adm-toolbar">
        <p className="small muted adm-intro">
          {t('An agent is a colleague of its own kind: assign it an issue, or mention it in a comment, and it prepares a draft pull request — or answers, when the issue is a question. It sees what its project roles allow, and a runner you operate does the work without ever holding your Git token or model key.')}
        </p>
        <span className="spacer" />
        <button className="btn btn-primary" onClick={() => setEditing('new')}><Plus size={15} /> {t('Add an agent')}</button>
      </div>
      {q.isLoading && <Loading />}
      {q.error && <ErrorBox error={q.error} />}
      {d && !d.modelConfigured && (
        <div className="ps-note" role="status"><TriangleAlert size={18} aria-hidden /><div>{t('No model is set up on this server (TASKCRAFT_AI_PROVIDER): agents can’t work until there is one.')}</div></div>
      )}
      {d && d.agents.length === 0 && <Empty icon={<Bot size={32} />} title={t('No agents yet')}>{t('Add one, give it a repository, and try it out on an issue before you open it to everyone.')}</Empty>}
      <div className="adm-hooks">
        {d?.agents.map((a) => (
          <div key={a.id} className={`adm-hook ${a.state === 'off' ? 'adm-off' : ''}`}>
            <div className="adm-hook-head">
              <Bot size={16} className="faint" aria-hidden />
              <strong className="adm-hook-url">{a.name}</strong>
              <span className="faint small">@{a.handle}</span>
              <span className={`badge ${a.state === 'active' ? 'badge-success' : a.state === 'testing' ? 'badge-warning' : ''}`}>{stateLabel(a.state)}</span>
            </div>
            <div className="adm-hook-events">
              <span className="label-chip">{a.repository ?? t('no repository: it only answers')}</span>
              {a.model && <span className="label-chip">{a.model}</span>}
            </div>
            <div className="adm-hook-foot">
              <span className="small faint">
                {t('{count} runs', { count: a.runs })}
                {a.activeRuns > 0 && <> · {t('{count} under way', { count: a.activeRuns })}</>}
                {a.tokensIn + a.tokensOut > 0 && <> · {t('{in} tokens in, {out} out', { in: tokens(a.tokensIn), out: tokens(a.tokensOut) })}</>}
              </span>
              <span className="spacer" />
              <button className="btn btn-sm" onClick={() => setRunsOf(a)}><ScrollText size={13} /> {t('Runs')}</button>
              <button className="btn btn-sm" onClick={() => setEditing(a)}><Pencil size={13} /> {t('Edit')}</button>
              <button className="icon-btn icon-btn-sm" title={t('Remove')} aria-label={t('Remove {name}', { name: a.name })} onClick={() => setRemoving(a)}><Trash size={14} /></button>
            </div>
          </div>
        ))}
      </div>

      {d && (
        <>
          <div className="adm-toolbar" style={{ marginTop: 22 }}>
            <h2 style={{ margin: 0, fontSize: 15.5 }}>{t('Runners')}</h2>
            <p className="small muted adm-intro">{t('The program that does the agents’ work, on a machine of yours — best in a container that holds nothing else.')}</p>
            <span className="spacer" />
            <button className="btn" onClick={() => setAddingRunner(true)}><Plus size={15} /> {t('Add a runner')}</button>
          </div>
          {d.runners.length === 0 && <p className="faint small">{t('No runner yet: runs wait until one is registered.')}</p>}
          <div className="adm-hooks">
            {d.runners.map((r) => (
              <div key={r.id} className="adm-hook">
                <div className="adm-hook-head">
                  <Server size={15} className="faint" aria-hidden />
                  <strong className="adm-hook-url">{r.name}</strong>
                  {!r.registeredAt && <span className="badge badge-warning">{t('Not registered yet')}</span>}
                  <span className="faint small" title={r.lastSeenAt ? dateTime(r.lastSeenAt) : undefined}>{r.lastSeenAt ? t('last seen {when}', { when: ago(r.lastSeenAt) }) : ''}</span>
                  <span className="spacer" />
                  <button className="icon-btn icon-btn-sm" title={t('Remove')} aria-label={t('Remove {name}', { name: r.name })} onClick={() => setRemovingRunner(r)}><Trash size={14} /></button>
                </div>
              </div>
            ))}
          </div>
        </>
      )}

      {editing && d && <AgentDialog agent={editing === 'new' ? null : editing} page={d} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); refresh() }} />}
      {runsOf && <RunsDialog agent={runsOf} onClose={() => setRunsOf(null)} />}
      {addingRunner && d && <RunnerDialog serverUrl={d.serverUrl} onClose={() => { setAddingRunner(false); refresh() }} />}
      {removing && (
        <Confirm
          title={t('Remove {name}?', { name: removing.name })}
          body={<p>{t('Its account is deactivated: what it wrote and the pull requests it opened stay.')}</p>}
          confirmLabel={t('Remove')}
          danger
          onClose={() => setRemoving(null)}
          onConfirm={async () => { await del(`/admin/agents/${removing.id}`); setRemoving(null) }}
        />
      )}
      {removingRunner && (
        <Confirm
          title={t('Remove {name}?', { name: removingRunner.name })}
          body={<p>{t('The runner can’t pick up work any more.')}</p>}
          confirmLabel={t('Remove')}
          danger
          onClose={() => setRemovingRunner(null)}
          onConfirm={async () => { await del(`/admin/agent-runners/${removingRunner.id}`); setRemovingRunner(null) }}
        />
      )}
    </>
  )
}

function AgentDialog({ agent, page, onClose, onSaved }: { agent: Agent | null; page: AgentsPage; onClose: () => void; onSaved: () => void }) {
  const [name, setName] = useState(agent?.name ?? '')
  const [handle, setHandle] = useState(agent?.handle ?? '')
  const [instructions, setInstructions] = useState(agent?.instructions ?? '')
  const [model, setModel] = useState(agent?.model ?? '')
  const [repo, setRepo] = useState(agent?.integrationId ?? '')
  const [base, setBase] = useState(agent?.baseBranch ?? '')
  const [state, setState] = useState<Agent['state']>(agent?.state ?? 'testing')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const save = async () => {
    setBusy(true)
    setErr('')
    const body = { name: name.trim(), handle: handle.trim(), instructions, model: model.trim() || null, integrationId: repo || null, baseBranch: base.trim() || null, state }
    try {
      if (agent) await api.patch(`/admin/agents/${agent.id}`, body)
      else await api.post('/admin/agents', body)
      onSaved()
    } catch (e) {
      setErr((e as Error).message)
      setBusy(false)
    }
  }
  const chosen = page.repositories.find((r) => r.id === repo)
  return (
    <Modal
      title={agent ? t('Edit {name}', { name: agent.name }) : t('Add an agent')}
      onClose={onClose}
      width={620}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !name.trim() || handle.trim().length < 2} onClick={save}>{t('Save')}</button>
        </>
      }
    >
      <div className="ap-grid">
        <div className="field">
          <label htmlFor="ag-name">{t('Name')}</label>
          <input id="ag-name" className="input" maxLength={60} value={name} autoFocus onChange={(e) => { setName(e.target.value); if (!agent && !handle) setHandle('') }} />
        </div>
        <div className="field">
          <label htmlFor="ag-handle">{t('Handle (for @mentions)')}</label>
          <input id="ag-handle" className="input" maxLength={30} value={handle} placeholder="cody" onChange={(e) => setHandle(e.target.value.toLowerCase().replace(/[^a-z0-9_-]/g, ''))} />
        </div>
      </div>
      <div className="field">
        <label htmlFor="ag-instructions">{t('Instructions')}</label>
        <textarea id="ag-instructions" className="textarea" rows={5} value={instructions} placeholder={t('How your team works: the language and style of the code, what to test, what never to touch.')} onChange={(e) => setInstructions(e.target.value)} />
      </div>
      <div className="ap-grid">
        <div className="field">
          <label htmlFor="ag-repo">{t('Repository')}</label>
          <select id="ag-repo" className="select" value={repo} onChange={(e) => setRepo(e.target.value)}>
            <option value="">{t('None: it only answers')}</option>
            {page.repositories.map((r) => <option key={r.id} value={r.id}>{r.repo} ({r.projectKey})</option>)}
          </select>
          <span className="field-hint">{t('Repositories connected with a token under Project settings → Integrations → Git.')}</span>
        </div>
        <div className="field">
          <label htmlFor="ag-base">{t('Base branch')}</label>
          <input id="ag-base" className="input" value={base} placeholder={chosen?.defaultBranch ?? 'main'} onChange={(e) => setBase(e.target.value)} />
        </div>
        <div className="field">
          <label htmlFor="ag-model">{t('Model (empty: the server’s)')}</label>
          <input id="ag-model" className="input" value={model} onChange={(e) => setModel(e.target.value)} />
        </div>
        <div className="field">
          <label htmlFor="ag-state">{t('Who can start it')}</label>
          <select id="ag-state" className="select" value={state} onChange={(e) => setState(e.target.value as Agent['state'])}>
            <option value="testing">{t('Only workspace admins (trying it out)')}</option>
            <option value="active">{t('Everyone who works on the issue')}</option>
            <option value="off">{t('Nobody (off)')}</option>
          </select>
        </div>
      </div>
      <p className="field-hint">{t('The agent is a member like anyone: add it to projects, or keep it out of them, under their members.')}</p>
      {err && <p className="error-text" role="alert">{err}</p>}
    </Modal>
  )
}

function RunnerDialog({ serverUrl, onClose }: { serverUrl: string; onClose: () => void }) {
  const toast = useToast()
  const [name, setName] = useState('')
  const [token, setToken] = useState('')
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState('')
  const add = async () => {
    setBusy(true)
    setErr('')
    try {
      setToken((await api.post<{ registrationToken: string }>('/admin/agent-runners', { name: name.trim() })).registrationToken)
    } catch (e) {
      setErr((e as Error).message)
    }
    setBusy(false)
  }
  const command = `taskcraft-runner register --server ${serverUrl} --token ${token}\ntaskcraft-runner run`
  return (
    <Modal
      title={t('Add a runner')}
      onClose={onClose}
      width={620}
      footer={token ? <button className="btn btn-primary" onClick={onClose}>{t('Done')}</button> : (
        <>
          <button className="btn" onClick={onClose}>{t('Cancel')}</button>
          <button className="btn btn-primary" disabled={busy || !name.trim()} onClick={add}>{t('Add a runner')}</button>
        </>
      )}
    >
      {!token ? (
        <div className="field">
          <label htmlFor="ag-runner">{t('Name')}</label>
          <input id="ag-runner" className="input" maxLength={60} autoFocus value={name} placeholder={t('e.g. build-box')} onChange={(e) => setName(e.target.value)} />
        </div>
      ) : (
        <>
          <p style={{ marginTop: 0 }}>{t('On the machine that will do the work, register the runner with this one-time token and start it. The token isn’t shown again.')}</p>
          <pre className="agent-log" style={{ marginLeft: 0 }} aria-label={t('Commands to register and start the runner')}>{command}</pre>
          <button className="btn btn-sm" onClick={() => void navigator.clipboard?.writeText(command).then(() => toast(t('Copied')), () => toast(t('Copying didn’t work: select the text instead')))}><Copy size={13} /> {t('Copy')}</button>
        </>
      )}
      {err && <p className="error-text" role="alert">{err}</p>}
    </Modal>
  )
}

function RunsDialog({ agent, onClose }: { agent: Agent; onClose: () => void }) {
  const open = useOpenIssue()
  const q = useQuery({ queryKey: ['admin', 'agent-runs', agent.id], queryFn: () => api.get<AgentRun[]>(`/admin/agents/${agent.id}/runs`) })
  return (
    <Modal title={t('Runs of {name}', { name: agent.name })} onClose={onClose} width={760} footer={<button className="btn btn-primary" onClick={onClose}>{t('Close')}</button>}>
      {q.isLoading && <Loading />}
      {q.error && <ErrorBox error={q.error} />}
      {q.data?.length === 0 && <p className="faint">{t('It hasn’t run yet. Assign it an issue or mention it in a comment.')}</p>}
      <div className="ext-logs">
        {q.data?.map((r) => (
          <div key={r.id} className="ext-log">
            <span className={`badge ${runStateBadge(r.state)}`}>{runStateLabel(r.state)}</span>
            <a href={`/issues/${r.issue.key}`} onClick={(e) => { e.preventDefault(); onClose(); open(r.issue.key) }}><span className="issue-key">{r.issue.key}</span></a>
            <span style={{ flex: 1, minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              {r.issue.title}
              {r.error && <span className="adm-bad"> — {r.error}</span>}
            </span>
            {r.result?.url && <a href={r.result.url} target="_blank" rel="noreferrer noopener">#{r.result.number}</a>}
            <span className="faint small" title={t('{in} tokens in, {out} out', { in: r.tokensIn, out: r.tokensOut })}>{tokens(r.tokensIn + r.tokensOut)}</span>
            <span className="faint small" title={dateTime(r.createdAt)}>{ago(r.createdAt)}</span>
          </div>
        ))}
      </div>
    </Modal>
  )
}
