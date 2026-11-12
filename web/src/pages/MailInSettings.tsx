// Project settings → E-mail: whether the project takes e-mail, at which address and from whom,
// how this server receives mail, and the latest messages with what became of them.
import { t } from '../i18n'
import { useState } from 'react'
import { Link } from 'react-router-dom'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Copy, Inbox, Mail, Server, TriangleAlert, Users, Globe } from 'lucide-react'
import { api, ago, dateTime, type Project } from '../api'
import { ErrorBox, Loading, useToast } from '../ui'
import './MailInSettings.css'

type Senders = 'members' | 'anyone'
interface Settings {
  enabled: boolean
  address?: string | null
  senders: Senders
}
interface Message {
  id: string
  receivedAt: string
  source: 'imap' | 'http'
  from: string
  fromName: string | null
  to: string | null
  subject: string
  status: 'processing' | 'created' | 'commented' | 'ignored' | 'rejected' | 'failed' | 'duplicate'
  reason: string | null
  detail: string | null
  issueKey: string | null
  routed: boolean
}
interface MailIn {
  settings: Settings
  imap: { mailbox: string; checkedAt: string | null; okAt: string | null; error: string | null } | null
  http: boolean
  endpoint: string
  smtp: boolean
  plusAddress: string | null
  messages: Message[]
}

const REASONS: Record<string, () => string> = {
  'auto-reply': () => t('an automatic reply (out of office and the like)'),
  bulk: () => t('bulk or mailing-list mail'),
  bounce: () => t('a delivery failure notice'),
  loop: () => t('sent by TaskCraft itself'),
  duplicate: () => t('received before'),
  'too-large': () => t('too large'),
  'no-sender': () => t('it has no sender address'),
  forged: () => t('the sender address failed the receiving server’s check (DMARC)'),
  'rate-limit': () => t('too many messages from this sender in the last hour'),
  'no-project': () => t('no project takes e-mail at this address'),
  'not-allowed': () => t('the sender may not write here'),
  archived: () => t('the project is archived'),
  empty: () => t('nothing new in it'),
  invalid: () => t('it couldn’t be saved'),
  unreadable: () => t('it isn’t a readable e-mail'),
  failing: () => t('handling it failed several times'),
  error: () => t('handling it failed; it is tried again'),
  interrupted: () => t('handling it was interrupted'),
}

const errText = (e: unknown) => (e instanceof Error ? e.message : String(e))

function Result({ m }: { m: Message }) {
  const why = m.reason ? (REASONS[m.reason]?.() ?? m.reason) : null
  const title = [why, m.detail].filter(Boolean).join(' · ')
  switch (m.status) {
    case 'created':
    case 'commented': {
      const label = m.status === 'created' ? t('New issue {key}', { key: m.issueKey ?? '' }) : t('Comment on {key}', { key: m.issueKey ?? '' })
      const badge = <span className="badge badge-success" title={m.detail ?? undefined}>{label}</span>
      return m.issueKey ? <Link to={`/issues/${m.issueKey}`} className="mailin-issue">{badge}</Link> : badge
    }
    case 'rejected':
      return <span className="badge badge-danger" title={title}>{t('Refused')}</span>
    case 'failed':
      return <span className="badge badge-warning" title={title}>{t('Failed')}</span>
    case 'processing':
      return <span className="badge">{t('In progress')}</span>
    default:
      return <span className="badge" title={title}>{m.status === 'duplicate' ? t('Duplicate') : t('Ignored')}</span>
  }
}

function CopyLine({ text, label }: { text: string; label: string }) {
  const toast = useToast()
  return (
    <span className="row mailin-copy">
      <code title={text}>{text}</code>
      <button
        type="button"
        className="icon-btn icon-btn-sm"
        title={t('Copy')}
        aria-label={label}
        onClick={() => void navigator.clipboard?.writeText(text).then(() => toast(t('Copied')), () => toast(t('Copying didn’t work: select the text instead')))}
      >
        <Copy size={13} />
      </button>
    </span>
  )
}

export default function MailInSettings({ project, onSaved }: { project: Project; onSaved: () => void }) {
  const qc = useQueryClient()
  const qk = ['mail-in', project.key]
  const q = useQuery({ queryKey: qk, queryFn: () => api.get<MailIn>(`/projects/${project.key}/mail-in`), refetchInterval: 60_000 })
  if (q.isLoading) return <Loading />
  if (q.error || !q.data) return <ErrorBox error={q.error} />
  return (
    <SettingsForm
      key={JSON.stringify(q.data.settings)}
      project={project}
      data={q.data}
      onSaved={(d) => {
        qc.setQueryData(qk, d)
        onSaved()
      }}
    />
  )
}

function SettingsForm({ project, data, onSaved }: { project: Project; data: MailIn; onSaved: (d: MailIn) => void }) {
  const toast = useToast()
  const [enabled, setEnabled] = useState(data.settings.enabled)
  const [address, setAddress] = useState(data.settings.address ?? '')
  const [senders, setSenders] = useState<Senders>(data.settings.senders)
  const [busy, setBusy] = useState(false)
  const dirty = enabled !== data.settings.enabled || address.trim() !== (data.settings.address ?? '') || senders !== data.settings.senders
  const validAddress = !address.trim() || /^[^\s@]+@[^\s@]+$/.test(address.trim())
  const configured = !!data.imap || data.http
  const save = async () => {
    setBusy(true)
    try {
      onSaved(await api.put<MailIn>(`/projects/${project.key}/mail-in`, { enabled, address: address.trim() || null, senders }))
      toast(t('Saved'))
    } catch (e) {
      toast(errText(e))
    } finally {
      setBusy(false)
    }
  }
  const reach = address.trim() || data.plusAddress
  return (
    <>
      <section className="ps-section">
        <div className="ps-section-head">
          <div className="ps-section-text">
            <h2><span className="row"><Mail size={17} aria-hidden /> {t('E-mail')}</span></h2>
            <p className="ps-desc">
              {t('Issues and comments by e-mail: a message to the project becomes an issue, a reply becomes a comment. Useful for support, and for anyone who’d rather write an e-mail.')}
            </p>
          </div>
        </div>

        {!configured && (
          <div className="ps-warn">
            <TriangleAlert size={18} aria-hidden />
            <div>
              {t('This server doesn’t receive e-mail yet. A server admin sets TASKCRAFT_IMAP_URL (a mailbox TaskCraft reads every minute) or TASKCRAFT_MAIL_IN_TOKEN (for a mail server that hands messages over); the README explains both.')}
            </div>
          </div>
        )}
        {data.imap && (
          <div className="ps-note">
            <Inbox size={18} aria-hidden />
            <div className="col ps-gap-2">
              <span>{t('TaskCraft reads the mailbox {mailbox} every minute.', { mailbox: data.imap.mailbox })}</span>
              {data.imap.error ? (
                <span className="error-text small"><TriangleAlert size={12} aria-hidden /> {t('Last check {when}: {error}', { when: ago(data.imap.checkedAt ?? ''), error: data.imap.error })}</span>
              ) : data.imap.okAt ? (
                <span className="faint small">{t('Last checked {when}', { when: ago(data.imap.okAt) })}</span>
              ) : (
                <span className="faint small">{t('Not checked yet')}</span>
              )}
            </div>
          </div>
        )}
        {data.http && (
          <div className="ps-note">
            <Server size={18} aria-hidden />
            <div className="col ps-gap-2">
              <span>{t('Mail servers and services can also hand messages to this address, with the token from TASKCRAFT_MAIL_IN_TOKEN:')}</span>
              <CopyLine text={data.endpoint} label={t('Copy the address for mail servers')} />
            </div>
          </div>
        )}

        <label className="ps-check mailin-enable">
          <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
          <span className="ps-strong">{t('Accept e-mail for {name}', { name: project.name })}</span>
        </label>

        <div className="field">
          <label htmlFor="mailin-address">{t('Inbound address')}</label>
          <input
            id="mailin-address"
            className="input"
            type="email"
            placeholder="support@example.com"
            value={address}
            disabled={!enabled}
            aria-invalid={!validAddress || undefined}
            onChange={(e) => setAddress(e.target.value)}
          />
          <span className="field-hint">
            {t('Mail to this address becomes issues here; deliver or forward it to the mailbox TaskCraft reads. Replies to TaskCraft’s e-mails about this project’s issues go to it as well.')}{' '}
            {data.plusAddress
              ? t('Without an address of its own, the project is reachable at {address}.', { address: data.plusAddress })
              : t('Without an address of its own, any address with +{key} before the @ (like inbox+{key}@your-domain) reaches the project.', { key: project.key })}
          </span>
        </div>

        <div className="field">
          <span className="field-label" id="mailin-senders">{t('Who may write')}</span>
          <div className="ps-kind-grid mailin-senders" role="radiogroup" aria-labelledby="mailin-senders">
            <button type="button" role="radio" aria-checked={senders === 'members'} disabled={!enabled} className={`ps-kind ${senders === 'members' ? 'on' : ''}`} onClick={() => setSenders('members')}>
              <Users size={15} aria-hidden /> {t('People with access')}
            </button>
            <button type="button" role="radio" aria-checked={senders === 'anyone'} disabled={!enabled} className={`ps-kind ${senders === 'anyone' ? 'on' : ''}`} onClick={() => setSenders('anyone')}>
              <Globe size={15} aria-hidden /> {t('Anyone')}
            </button>
          </div>
          <span className="field-hint">
            {senders === 'members'
              ? t('People write with the e-mail address of their account and act as themselves: members create issues, viewers can comment. Mail from anyone else is refused.')
              : t('Also people without an account, as for a support desk: each becomes the requester of the issue they open, gets an acknowledgement with its key and can answer by e-mail. Comments you mark for them are e-mailed.')}
          </span>
        </div>

        {enabled && senders === 'anyone' && !data.smtp && (
          <p className="ps-warn-text small">{t('Outgoing e-mail isn’t set up (TASKCRAFT_SMTP_URL): requesters get no acknowledgement and no replies.')}</p>
        )}

        <div className="row">
          <button className="btn btn-primary" disabled={!dirty || !validAddress || busy} onClick={save}>{t('Save')}</button>
          {enabled && reach && <span className="faint small">{t('Try it: send an e-mail to {address}.', { address: reach })}</span>}
        </div>

        <ul className="mailin-how faint small">
          <li>{t('Replies to TaskCraft’s e-mails, and messages with the key in brackets in the subject (like [{key}-12]), become comments on that issue; quoted text and signatures are left out.', { key: project.key })}</li>
          <li>{t('Other messages become new issues: the subject is the title, the text the description, files become attachments.')}</li>
          <li>{t('Automatic replies, bulk and mailing-list mail, delivery failure notices and messages TaskCraft sent itself are ignored.')}</li>
        </ul>
      </section>

      <section className="ps-section">
        <div className="ps-section-head">
          <div className="ps-section-text">
            <h2>{t('Received')}</h2>
            <p className="ps-desc">{t('The latest messages for this project and what became of them. Hover over a result for details.')}</p>
          </div>
        </div>
        {data.messages.length === 0 ? (
          <p className="faint">{t('Nothing received yet.')}</p>
        ) : (
          <div className="mailin-table">
            <table className="list">
              <thead>
                <tr>
                  <th>{t('Received')}</th>
                  <th>{t('From')}</th>
                  <th>{t('Subject')}</th>
                  <th>{t('Result')}</th>
                </tr>
              </thead>
              <tbody>
                {data.messages.map((m) => {
                  const why = m.reason ? (REASONS[m.reason]?.() ?? m.reason) : null
                  return (
                    <tr key={m.id}>
                      <td className="nowrap" title={dateTime(m.receivedAt)}>{ago(m.receivedAt)}</td>
                      <td className="mailin-from">
                        <span>{m.fromName ?? m.from}</span>
                        {m.fromName && <span className="faint small">{m.from}</span>}
                      </td>
                      <td className="mailin-subject">
                        <span>{m.subject || <span className="faint">{t('(no subject)')}</span>}</span>
                        {!m.routed && (
                          <span className="faint small">
                            {m.reason === 'no-project' && m.detail ? t('Reached no project (sent to {to})', { to: m.detail }) : t('Reached no project')}
                          </span>
                        )}
                      </td>
                      <td>
                        <Result m={m} />
                        {why && m.status !== 'created' && m.status !== 'commented' && <div className="faint small">{why}</div>}
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </>
  )
}
