import { useInfiniteQuery } from '@tanstack/react-query'
import { Bot, Server, User, UserX } from 'lucide-react'
import { Link } from 'react-router'

import { api, type AuditEvent } from '../api'
import { usePhone } from '../phone'
import { useMe } from '../session'
import { formatAgo } from './format'

// Activity is the audit log narrowed to what concerns the subjects given,
// newest first: pageSize events, and pageSize more each time Load more is
// pressed. On a phone each event is a row of a list.
export function Activity({
  subjects,
  empty = 'Nothing recorded yet.',
  pageSize = 50,
}: {
  subjects: string[]
  empty?: string
  pageSize?: number
}) {
  const log = useInfiniteQuery({
    queryKey: ['audit', ...subjects, pageSize],
    queryFn: ({ pageParam }) => api.audit(subjects, pageParam, pageSize),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (last) => (last.length === pageSize ? last[last.length - 1].id : undefined),
    refetchInterval: 10000,
  })
  const phone = usePhone()
  if (log.isPending) return <div className="panel empty muted">Loading…</div>
  if (log.isError) return <div className="alert">{log.error.message}</div>
  const events = log.data.pages.flat()
  if (phone) {
    return (
      <div className="phone-list">
        {events.length === 0 && <div className="phone-row phone-row-empty">{empty}</div>}
        {events.map((e) => (
          <PhoneEvent key={e.id} event={e} />
        ))}
        {log.hasNextPage && (
          <button
            type="button"
            className="phone-row phone-row-accent"
            onClick={() => log.fetchNextPage()}
            disabled={log.isFetchingNextPage}
          >
            <span className="phone-row-main">{log.isFetchingNextPage ? 'Loading…' : 'Load more'}</span>
          </button>
        )}
      </div>
    )
  }
  return (
    <div className="panel">
      {events.length === 0 ? (
        <div className="empty">{empty}</div>
      ) : (
        <table className="table activity">
          <tbody>
            {events.map((e) => (
              <Row key={e.id} event={e} />
            ))}
          </tbody>
        </table>
      )}
      {log.hasNextPage && (
        <div className="form-actions activity-more">
          <button
            type="button"
            className="btn btn-ghost"
            onClick={() => log.fetchNextPage()}
            disabled={log.isFetchingNextPage}
          >
            {log.isFetchingNextPage ? 'Loading…' : 'Load more'}
          </button>
        </div>
      )}
    </div>
  )
}

const actorIcon = { person: User, system: Bot, worker: Server, anonymous: UserX }

function Row({ event: e }: { event: AuditEvent }) {
  const me = useMe()
  const Icon = actorIcon[e.actor.kind]
  return (
    <tr>
      <td className="nowrap muted small" title={new Date(e.at).toLocaleString()}>
        {formatAgo(e.at)}
      </td>
      <td className="nowrap">
        <span className={`actor actor-${e.actor.kind}`} title={e.actor.kind}>
          <Icon size={13} />
          {e.actor.name || 'someone'}
        </span>
      </td>
      <td>
        <span className="strong">{describe(e.action)}</span>{' '}
        {me.admin && e.target.id ? (
          <Link to={`/admin/audit?subject=${encodeURIComponent(`${e.target.type}:${e.target.id}`)}`} className="mono">
            {e.target.name || e.target.id}
          </Link>
        ) : (
          <span className="mono">{e.target.name || e.target.id}</span>
        )}
        <Details details={e.details} />
      </td>
      {me.admin && <td className="muted small mono nowrap">{e.ip}</td>}
    </tr>
  )
}

function PhoneEvent({ event: e }: { event: AuditEvent }) {
  const me = useMe()
  const target = e.target.name || e.target.id
  return (
    <div className="phone-row phone-event">
      <span className="phone-row-main">
        <span className="phone-event-head">
          <span className="strong">{e.actor.name || 'someone'}</span>
          <span className="phone-row-sub" title={new Date(e.at).toLocaleString()}>
            {formatAgo(e.at)}
          </span>
        </span>
        <span>
          {describe(e.action)}{' '}
          {me.admin && e.target.id ? (
            <Link to={`/admin/audit?subject=${encodeURIComponent(`${e.target.type}:${e.target.id}`)}`} className="mono">
              {target}
            </Link>
          ) : (
            <span className="mono">{target}</span>
          )}
        </span>
        <Details details={e.details} />
        {me.admin && e.ip && <span className="phone-row-sub mono">{e.ip}</span>}
      </span>
    </div>
  )
}

function Details({ details }: { details: Record<string, unknown> }) {
  const parts = Object.entries(details).filter(([, v]) => v !== '' && v !== null && v !== undefined)
  if (parts.length === 0) return null
  return (
    <div className="muted small">
      {parts.map(([k, v]) => (
        <span key={k} className="detail">
          {k.replaceAll('_', ' ')}: {typeof v === 'object' ? JSON.stringify(v) : String(v)}
        </span>
      ))}
    </div>
  )
}

// describe turns noun.verb into words.
function describe(action: string): string {
  const [noun, verb = ''] = action.split('.')
  return `${noun.replaceAll('_', ' ')} ${verb.replaceAll('_', ' ')}`.trim()
}
