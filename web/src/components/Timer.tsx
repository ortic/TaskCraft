import { t } from '../i18n'
import { useEffect, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Pause, Play, Square, Timer as TimerIcon, Trash, FileClock } from 'lucide-react'
import { api, duration, type Issue } from '../api'
import { Confirm, Modal, Popover, useFeature, useToast } from '../ui'
import { CategorySelect, useIssue, useTimeCategories } from './IssueView'
import { useOpenIssue } from './Layout'

export interface RunningTimer {
  issue: Issue | null
  startedAt: string
  pausedAt: string | null
  elapsedSeconds: number
  note: string | null
}

/** A stopped timer's time, waiting to be logged (only its owner sees it). */
export interface TimeDraft {
  id: string
  key: string
  title: string
  minutes: number
  date: string
  startedAt: string | null
  note: string | null
  createdAt: string
}

/** The caller's running timer and how many drafts wait (live: kept on the server). */
export function useTimer() {
  const on = useFeature('timeTracking')
  return useQuery({
    queryKey: ['timer'],
    queryFn: () => api.get<{ timer: RunningTimer | null; drafts: number }>('/me/timer'),
    enabled: on,
    staleTime: 30_000,
    refetchOnWindowFocus: true,
  })
}

/** Seconds the timer has run, ticking while it runs. */
function useElapsed(timer: RunningTimer | null | undefined, fetchedAt: number) {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!timer || timer.pausedAt) return
    const id = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(id)
  }, [timer])
  if (!timer) return 0
  return timer.elapsedSeconds + (timer.pausedAt ? 0 : Math.max(0, Math.floor((now - fetchedAt) / 1000)))
}

export function clock(seconds: number) {
  const h = Math.floor(seconds / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  const s = seconds % 60
  const p = (n: number) => String(n).padStart(2, '0')
  return `${h}:${p(m)}:${p(s)}`
}

function useTimerActions() {
  const qc = useQueryClient()
  const toast = useToast()
  const refresh = () => {
    qc.invalidateQueries({ queryKey: ['timer'] })
    qc.invalidateQueries({ queryKey: ['time-drafts'] })
  }
  const run = async <T,>(fn: () => Promise<T>): Promise<T | undefined> => {
    try {
      return await fn()
    } catch (e) {
      toast((e as Error).message)
    } finally {
      refresh()
    }
  }
  return {
    pause: () => run(() => api.post('/me/timer/pause', {})),
    resume: () => run(() => api.post('/me/timer/resume', {})),
    /** Stops into a draft (the dialog then logs it). */
    stop: () => run(() => api.post<{ draft: TimeDraft | null }>('/me/timer/stop', {})).then((r) => r?.draft ?? null),
    discard: () => run(() => api.del('/me/timer')),
  }
}

/** The timer in the top bar: the issue and the time ticking, pause / stop; drafts waiting. */
export function TimerChip() {
  const timer = useTimer()
  const [el, setEl] = useState<HTMLButtonElement | null>(null)
  const [open, setOpen] = useState(false)
  const [booking, setBooking] = useState<TimeDraft | null>(null)
  const [discarding, setDiscarding] = useState(false)
  const actions = useTimerActions()
  const openIssue = useOpenIssue()
  const running = timer.data?.timer ?? null
  const elapsed = useElapsed(running, timer.dataUpdatedAt)
  const drafts = timer.data?.drafts ?? 0
  if (!running && !drafts && !booking) return null
  return (
    <>
      <button
        ref={setEl}
        className={`btn btn-sm timer-chip${running ? (running.pausedAt ? ' is-paused' : ' is-running') : ''}`}
        onClick={() => setOpen((o) => !o)}
        aria-label={running ? t('Timer on {key}: {time}', { key: running.issue?.key ?? '', time: clock(elapsed) }) : t('{n} time drafts', { n: drafts })}
        title={running ? running.issue?.title : t('Time drafts to log')}
      >
        {running ? <TimerIcon size={14} /> : <FileClock size={14} />}
        {running && <span className="issue-key">{running.issue?.key}</span>}
        {running && <span className="timer-clock">{clock(elapsed)}</span>}
        {drafts > 0 && <span className="badge" aria-hidden>{drafts}</span>}
      </button>
      {open && (
        <Popover anchor={el} onClose={() => setOpen(false)} align="end" width={340}>
          {running && (
            <div className="timer-pop">
              <button className="timer-issue" onClick={() => { setOpen(false); running.issue && openIssue(running.issue.key) }}>
                <span className="issue-key">{running.issue?.key}</span> {running.issue?.title}
              </button>
              <div className="timer-big" aria-live="off">{clock(elapsed)}{running.pausedAt && <span className="faint small"> · {t('paused')}</span>}</div>
              {running.note && <div className="faint small">{running.note}</div>}
              <div className="row" style={{ gap: 6, marginTop: 8 }}>
                {running.pausedAt ? (
                  <button className="btn btn-sm" onClick={() => actions.resume()}><Play size={14} /> {t('Resume')}</button>
                ) : (
                  <button className="btn btn-sm" onClick={() => actions.pause()}><Pause size={14} /> {t('Pause')}</button>
                )}
                <button
                  className="btn btn-sm btn-primary"
                  onClick={async () => {
                    const d = await actions.stop()
                    setOpen(false)
                    if (d) setBooking(d)
                  }}
                >
                  <Square size={13} /> {t('Stop and log…')}
                </button>
                <span className="spacer" />
                <button className="icon-btn" title={t('Discard: stop without keeping the time')} aria-label={t('Discard the timer')} onClick={() => setDiscarding(true)}><Trash size={15} /></button>
              </div>
            </div>
          )}
          {drafts > 0 && <DraftList onBook={(d) => { setOpen(false); setBooking(d) }} />}
        </Popover>
      )}
      {booking && <TimeDraftDialog draft={booking} onClose={() => setBooking(null)} />}
      {discarding && (
        <Confirm
          title={t('Discard the timer?')}
          body={<p>{t('The {time} timed on {key} are not kept.', { time: clock(elapsed), key: running?.issue?.key ?? '' })}</p>}
          confirmLabel={t('Discard')}
          danger
          onConfirm={async () => { await actions.discard(); setOpen(false) }}
          onClose={() => setDiscarding(false)}
        />
      )}
    </>
  )
}

function DraftList({ onBook }: { onBook: (d: TimeDraft) => void }) {
  const list = useQuery({ queryKey: ['time-drafts'], queryFn: () => api.get<TimeDraft[]>('/me/time-drafts') })
  const qc = useQueryClient()
  return (
    <div className="timer-drafts">
      <div className="menu-head">{t('Time drafts')}</div>
      <p className="faint small" style={{ margin: '0 10px 6px' }}>{t('Stopped timers wait here until you log them. Only you see them; reports don’t count them yet.')}</p>
      {(list.data ?? []).map((d) => (
        <div key={d.id} className="row timer-draft">
          <span className="issue-key">{d.key}</span>
          <span className="timer-draft-title">{d.title}</span>
          <span className="faint small">{duration(d.minutes)}</span>
          <button className="btn btn-sm" onClick={() => onBook(d)}>{t('Log…')}</button>
          <button
            className="icon-btn"
            title={t('Discard')}
            aria-label={t('Discard the draft for {key}', { key: d.key })}
            onClick={async () => {
              await api.del(`/me/time-drafts/${d.id}`)
              qc.invalidateQueries({ queryKey: ['time-drafts'] })
              qc.invalidateQueries({ queryKey: ['timer'] })
            }}
          >
            <Trash size={14} />
          </button>
        </div>
      ))}
    </div>
  )
}

/** Logs a draft: the timed time (it can be changed), the day, a note, the category and the remaining estimate. */
export function TimeDraftDialog({ draft, onClose }: { draft: TimeDraft; onClose: () => void }) {
  const qc = useQueryClient()
  const toast = useToast()
  const issue = useIssue(draft.key).data
  const categories = useTimeCategories().data ?? []
  const [time, setTime] = useState(duration(draft.minutes))
  const [date, setDate] = useState(draft.date)
  const [note, setNote] = useState(draft.note ?? '')
  const [category, setCategory] = useState('')
  const [billable, setBillable] = useState(false)
  const [remaining, setRemaining] = useState<'auto' | 'keep'>('auto')
  const [busy, setBusy] = useState(false)
  const hasEstimate = issue ? issue.estimateMinutes != null || !!issue.remainingSet : false
  const done = () => {
    for (const k of ['timer', 'time-drafts', 'timesheet', 'worklogs', 'issue']) qc.invalidateQueries({ queryKey: [k] })
    onClose()
  }
  return (
    <Modal
      title={t('Log the time on {key}', { key: draft.key })}
      onClose={onClose}
      width={480}
      footer={
        <>
          <button className="btn" onClick={onClose}>{t('Later')}</button>
          <button
            className="btn btn-primary"
            disabled={busy || !time.trim()}
            onClick={async () => {
              setBusy(true)
              try {
                await api.post(`/issues/${draft.key}/worklogs`, { time, date, note, categoryId: category || null, billable, remaining: hasEstimate ? remaining : undefined, draftId: draft.id })
                done()
              } catch (e) {
                toast((e as Error).message)
              } finally {
                setBusy(false)
              }
            }}
          >
            {t('Log time')}
          </button>
        </>
      }
    >
      <p className="faint small" style={{ marginTop: 0 }}>{draft.title}</p>
      <div className="row" style={{ gap: 10, alignItems: 'flex-start' }}>
        <div className="field" style={{ flex: 1 }}>
          <label htmlFor="td-time">{t('Time spent')}</label>
          <input id="td-time" className="input" value={time} onChange={(e) => setTime(e.target.value)} placeholder={t('1h 30m')} autoFocus />
        </div>
        <div className="field" style={{ flex: 1 }}>
          <label htmlFor="td-date">{t('Date')}</label>
          <input id="td-date" type="date" className="input" value={date} onChange={(e) => setDate(e.target.value)} />
        </div>
      </div>
      <div className="field">
        <label htmlFor="td-note">{t('Note')}</label>
        <input id="td-note" className="input" value={note} onChange={(e) => setNote(e.target.value)} placeholder={t('What did you do? (optional)')} />
      </div>
      <div className="row-wrap" style={{ gap: 10 }}>
        {categories.some((c) => !c.archived) && (
          <CategorySelect
            id="td-cat"
            categories={categories}
            value={category}
            onChange={(id) => {
              setCategory(id)
              setBillable(categories.find((c) => c.id === id)?.billable ?? false)
            }}
          />
        )}
        <label className="row small" style={{ gap: 6 }}>
          <input type="checkbox" checked={billable} onChange={(e) => setBillable(e.target.checked)} /> {t('Billable')}
        </label>
        {hasEstimate && (
          <select className="select input-sm" style={{ width: 'auto' }} value={remaining} onChange={(e) => setRemaining(e.target.value as typeof remaining)} aria-label={t('Remaining estimate')}>
            <option value="auto">{t('Reduce the remaining estimate')}</option>
            <option value="keep">{t('Keep the remaining estimate')}</option>
          </select>
        )}
      </div>
    </Modal>
  )
}

/** On an issue: start a timer, or stop the one running on it. */
export function TimerButton({ issueKey }: { issueKey: string }) {
  const timer = useTimer()
  const qc = useQueryClient()
  const toast = useToast()
  const actions = useTimerActions()
  const [booking, setBooking] = useState<TimeDraft | null>(null)
  const [other, setOther] = useState<string | null>(null)
  const running = timer.data?.timer ?? null
  const here = running?.issue?.key === issueKey
  const start = async (replace: boolean) => {
    try {
      const r = await api.post<{ draft: TimeDraft | null }>(`/issues/${issueKey}/timer`, { replace })
      if (r.draft) toast(t('The time on {key} waits as a draft', { key: r.draft.key }))
    } catch (e) {
      toast((e as Error).message)
    } finally {
      qc.invalidateQueries({ queryKey: ['timer'] })
    }
  }
  return (
    <>
      {here ? (
        <button
          className="btn btn-sm timer-start is-running"
          onClick={async () => {
            const d = await actions.stop()
            if (d) setBooking(d)
          }}
        >
          <Square size={13} /> {t('Stop timer')}
        </button>
      ) : (
        <button className="btn btn-sm timer-start" onClick={() => (running ? setOther(running.issue?.key ?? '') : start(false))}>
          <Play size={13} /> {t('Start timer')}
        </button>
      )}
      {booking && <TimeDraftDialog draft={booking} onClose={() => setBooking(null)} />}
      {other !== null && (
        <Confirm
          title={t('A timer runs on {key}', { key: other })}
          body={<p>{t('Stop it and start one here? Its time waits as a draft for you to log.')}</p>}
          confirmLabel={t('Start here')}
          onConfirm={() => start(true)}
          onClose={() => setOther(null)}
        />
      )}
    </>
  )
}
