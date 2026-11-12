// A schedule ("every weekday at 08:00") for automation rules and recurring issues. Times are
// local to the schedule's time zone (the browser's when created).
import { t, locale } from '../i18n'

export interface Schedule {
  every: 'hour' | 'day' | 'weekday' | 'week' | 'month'
  at: string
  weekday?: number
  day?: number
  tz?: string
}

export function browserZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC'
  } catch {
    return 'UTC'
  }
}

export const defaultSchedule = (): Schedule => ({ every: 'weekday', at: '08:00', weekday: 1, day: 1, tz: browserZone() })

function weekdayName(n: number): string {
  // 2024-01-01 was a Monday
  return new Intl.DateTimeFormat(locale(), { weekday: 'long' }).format(new Date(2024, 0, n))
}

export function describeSchedule(s: Schedule | null | undefined): string {
  if (!s) return ''
  const zone = s.tz && s.tz !== browserZone() ? ` (${s.tz})` : ''
  switch (s.every) {
    case 'hour':
      return t('every hour at minute {m}', { m: s.at.slice(3) })
    case 'day':
      return t('every day at {time}', { time: s.at }) + zone
    case 'weekday':
      return t('every weekday at {time}', { time: s.at }) + zone
    case 'week':
      return t('every {weekday} at {time}', { weekday: weekdayName(s.weekday ?? 1), time: s.at }) + zone
    case 'month':
      return t('on day {day} of every month at {time}', { day: s.day ?? 1, time: s.at }) + zone
  }
}

export default function ScheduleEditor({ value, onChange }: { value: Schedule; onChange: (s: Schedule) => void }) {
  const set = (p: Partial<Schedule>) => onChange({ ...value, tz: value.tz || browserZone(), ...p })
  return (
    <span className="row-wrap" style={{ gap: 6 }}>
      <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('How often')} value={value.every} onChange={(e) => set({ every: e.target.value as Schedule['every'] })}>
        <option value="hour">{t('every hour')}</option>
        <option value="day">{t('every day')}</option>
        <option value="weekday">{t('every weekday (Mon–Fri)')}</option>
        <option value="week">{t('every week')}</option>
        <option value="month">{t('every month')}</option>
      </select>
      {value.every === 'week' && (
        <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Weekday')} value={value.weekday ?? 1} onChange={(e) => set({ weekday: Number(e.target.value) })}>
          {[1, 2, 3, 4, 5, 6, 7].map((d) => <option key={d} value={d}>{t('on {weekday}', { weekday: weekdayName(d) })}</option>)}
        </select>
      )}
      {value.every === 'month' && (
        <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Day of the month')} value={value.day ?? 1} onChange={(e) => set({ day: Number(e.target.value) })}>
          {Array.from({ length: 28 }, (_, i) => i + 1).map((d) => <option key={d} value={d}>{t('on day {day}', { day: d })}</option>)}
        </select>
      )}
      {value.every === 'hour' ? (
        <select className="select input-sm" style={{ width: 'auto' }} aria-label={t('Minute')} value={value.at.slice(3)} onChange={(e) => set({ at: `00:${e.target.value}` })}>
          {['00', '15', '30', '45'].map((m) => <option key={m} value={m}>{t('at minute {m}', { m })}</option>)}
        </select>
      ) : (
        <>
          <span className="faint">{t('at')}</span>
          <input type="time" className="input input-sm" style={{ width: 110 }} aria-label={t('Time')} value={value.at} onChange={(e) => set({ at: e.target.value || '08:00' })} />
        </>
      )}
      <span className="faint small">{value.tz || browserZone()}</span>
    </span>
  )
}
