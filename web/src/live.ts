// Live updates: the server announces changes (Server-Sent Events); the screens showing them refetch.
import { useEffect } from 'react'
import { useQueryClient } from '@tanstack/react-query'

interface Change {
  kind: string
  projectId?: string | null
  issueId?: string | null
  key?: string | null
  actorId?: string | null
}

export function useLiveUpdates(enabled: boolean) {
  const qc = useQueryClient()
  useEffect(() => {
    if (!enabled) return
    let es: EventSource | null = null
    let retry: ReturnType<typeof setTimeout> | null = null
    let pending = new Set<string>()
    let flush: ReturnType<typeof setTimeout> | null = null
    const invalidate = (keys: string[]) => {
      keys.forEach((k) => pending.add(k))
      if (flush) return
      // a burst of changes (bulk edits, sprint completion) becomes one refetch
      flush = setTimeout(() => {
        const ks = pending
        pending = new Set()
        flush = null
        ks.forEach((k) => qc.invalidateQueries({ queryKey: [k] }))
      }, 250)
    }
    const connect = () => {
      es = new EventSource('/api/events')
      es.addEventListener('change', (ev) => {
        let c: Change
        try {
          c = JSON.parse((ev as MessageEvent).data)
        } catch {
          return
        }
        switch (c.kind) {
          case 'issue':
          case 'comment':
            invalidate(['issue', 'issues', 'board', 'backlog', 'search', 'my-work', 'comments', 'history', 'children', 'activity', 'report', 'timeline', 'calendar'])
            break
          case 'sprint':
            invalidate(['backlog', 'board', 'project', 'sprints', 'report'])
            break
          case 'board':
            invalidate(['board'])
            break
          case 'project':
            invalidate(['project', 'projects', 'board', 'backlog'])
            break
          case 'notification':
            invalidate(['notifications'])
            break
        }
      })
      es.addEventListener('resync', () => qc.invalidateQueries())
      es.onerror = () => {
        es?.close()
        retry = setTimeout(connect, 5000)
      }
    }
    connect()
    return () => {
      es?.close()
      if (retry) clearTimeout(retry)
      if (flush) clearTimeout(flush)
    }
  }, [enabled, qc])
}
