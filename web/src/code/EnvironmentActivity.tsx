import { useInfiniteQuery } from '@tanstack/react-query'
import { useState } from 'react'

import { api, type AuditEvent } from '../api'
import { formatAgo, formatMemory, shortDigest } from '../components/format'
import { useMe } from '../session'
import { homeTab, otherTab } from '../workspaceLinks'

// Events that say someone looked, rather than that anything changed, and
// the worker's account of each step of a start. The audit log keeps them;
// the environment's own page leaves them out.
const quiet = new Set([
  'environment.open_code',
  'environment.open_editor',
  'environment.open_desktop',
  'environment.open_ports',
  'environment.terminal_attach',
  'environment.terminal_close',
  'environment.ssh_connect',
  'environment.ssh_disconnect',
  'environment.place',
])

const notable = (e: AuditEvent) =>
  !quiet.has(e.action) && !(e.action === 'environment.phase' && e.details.to !== 'failed')

const pageSize = 50
const shownAtFirst = 6

// EnvironmentActivity is what has been done to an environment, newest
// first, each event a sentence: who, what, and the change it made.
export function EnvironmentActivity({ env }: { env: string }) {
  const me = useMe()
  const subject = `environment:${env}`
  const log = useInfiniteQuery({
    queryKey: ['audit', subject, pageSize],
    queryFn: ({ pageParam }) => api.audit([subject], pageParam, pageSize),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (last) => (last.length === pageSize ? last[last.length - 1].id : undefined),
    refetchInterval: 10000,
  })
  const [shown, setShown] = useState(shownAtFirst)
  if (log.isPending) return <p className="muted">Loading…</p>
  if (log.isError) return <div className="alert">{log.error.message}</div>
  const events = log.data.pages.flat().filter(notable)
  const more = events.length > shown || log.hasNextPage

  return (
    <div className="envtab-activity">
      {events.length === 0 && <p className="muted">Nothing done to it yet.</p>}
      {events.slice(0, shown).map((e) => (
        <Event key={e.id} event={e} />
      ))}
      <div className="envtab-activity-foot">
        {more && (
          <button
            type="button"
            className="envtab-link"
            disabled={log.isFetchingNextPage}
            onClick={() => {
              setShown((n) => n + 10)
              if (events.length <= shown + 10 && log.hasNextPage) log.fetchNextPage()
            }}
          >
            {log.isFetchingNextPage ? 'Loading…' : 'Show more'}
          </button>
        )}
        {me.admin && (
          <a href={`/admin/audit?subject=${encodeURIComponent(subject)}`} target={otherTab(homeTab)}>
            Everything, in the audit log
          </a>
        )}
      </div>
    </div>
  )
}

function Event({ event: e }: { event: AuditEvent }) {
  const who =
    e.actor.kind === 'person' ? e.actor.name || 'Someone' : e.actor.kind === 'worker' ? e.actor.name : 'Hangar'
  const { what, detail } = describe(e)
  return (
    <div className="envtab-event">
      <span className={`envtab-event-who envtab-event-${e.actor.kind}`} aria-hidden="true">
        {who.slice(0, 1).toUpperCase()}
      </span>
      <div className="envtab-event-main">
        <div>
          <strong>{who}</strong> {what}
        </div>
        {detail && <div className="muted small">{detail}</div>}
      </div>
      <span
        className="envtab-event-at muted small"
        title={new Date(e.at).toLocaleString() + (e.ip ? ` · from ${e.ip}` : '')}
      >
        {formatAgo(e.at)}
      </span>
    </div>
  )
}

type Change<T> = { from: T; to: T }

const settingWords: { key: string; label: string; show: (v: never) => string }[] = [
  { key: 'cpus', label: 'vCPUs', show: (v: number) => String(v) },
  { key: 'memory_mib', label: 'memory', show: (v: number) => formatMemory(v) },
  { key: 'display', label: 'display', show: (v: string) => (v === 'none' ? 'headless' : v) },
  { key: 'gpu', label: 'GPU', show: (v: string) => v },
  { key: 'dax', label: 'image mapping', show: (v: boolean) => (v ? 'on' : 'off') },
]

// describe says an event in words: what was done, and what it changed.
function describe(e: AuditEvent): { what: string; detail?: string } {
  const d = e.details
  switch (e.action) {
    case 'environment.create':
      return {
        what: 'created it',
        detail:
          typeof d.cpus === 'number' && typeof d.memory_mib === 'number'
            ? `${d.cpus} vCPUs · ${formatMemory(d.memory_mib)}`
            : undefined,
      }
    case 'environment.start':
      return { what: d.from === 'suspended' ? 'resumed it' : 'started it' }
    case 'environment.stop':
      return { what: 'stopped it' }
    case 'environment.suspend':
      return { what: 'suspended it' }
    case 'environment.delete':
    case 'environment.deleted':
      return { what: 'deleted it' }
    case 'environment.update_settings': {
      const changed = settingWords.flatMap(({ key, label, show }) => {
        const c = d[key] as Change<never> | undefined
        return c && c.from !== c.to ? [`${label} ${show(c.from)} → ${show(c.to)}`] : []
      })
      return { what: 'changed its settings', detail: changed.join(' · ') || undefined }
    }
    case 'environment.reset_to_template':
      return { what: 'reset it to its template' }
    case 'environment.ports_public':
      return { what: d.public ? 'made its web servers public' : 'made its web servers private' }
    case 'environment.image_upgrade':
      return {
        what: 'chose to upgrade its image at its next start',
        detail: typeof d.to === 'string' ? `to ${shortDigest(d.to)}` : undefined,
      }
    case 'environment.image_rollback':
      return { what: 'chose to roll its image back at its next start' }
    case 'environment.image_keep':
      return { what: 'kept its upgraded image' }
    case 'environment.image_change_cancel':
      return { what: 'cancelled the change to its image' }
    case 'environment.choose_copy':
      return { what: 'chose which copy of a pack it uses' }
    case 'environment.resolve_conflict':
      return { what: 'settled a file that became shared' }
    case 'environment.unplaceable':
      return { what: 'could not place it yet', detail: typeof d.reason === 'string' ? d.reason : undefined }
    case 'environment.phase':
      return { what: 'reported it failed', detail: typeof d.reason === 'string' ? d.reason : undefined }
    case 'environment.signal_process':
      return {
        what: `sent ${typeof d.signal === 'string' ? d.signal : 'a signal'} to process ${String(d.pid ?? '')}`.trim(),
        detail: typeof d.error === 'string' && d.error ? d.error : undefined,
      }
    case 'environment.ssh_refused':
      return { what: 'was refused SSH' }
  }
  const [, verb = e.action] = e.action.split('.')
  return { what: verb.replaceAll('_', ' ') }
}
