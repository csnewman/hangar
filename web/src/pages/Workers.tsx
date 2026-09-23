import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { api, type Worker } from '../api'
import { ConfirmButton } from '../components/ConfirmButton'
import { formatAgo, formatMemory } from '../components/format'
import { OnlineBadge } from '../components/Status'

export function WorkersPage() {
  const workers = useQuery({ queryKey: ['workers'], queryFn: api.workers })

  return (
    <section>
      <div className="page-head">
        <div>
          <h1>Workers</h1>
          <p className="muted">Machines that run environments. Each dials in to the control plane.</p>
        </div>
      </div>

      {workers.isError && <div className="alert">Could not load workers: {workers.error.message}</div>}

      <div className="panel">
        <table className="table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Status</th>
              <th>CPU</th>
              <th>Memory</th>
              <th>Labels</th>
              <th>Last seen</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {workers.data?.map((w) => <WorkerRow key={w.id} worker={w} />)}
            {workers.data?.length === 0 && (
              <tr>
                <td colSpan={7} className="empty">
                  No workers have registered.
                </td>
              </tr>
            )}
            {workers.isPending && (
              <tr>
                <td colSpan={7} className="empty">
                  Loading…
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </section>
  )
}

function WorkerRow({ worker: w }: { worker: Worker }) {
  const qc = useQueryClient()
  const refresh = () => qc.invalidateQueries({ queryKey: ['workers'] })
  const revoke = useMutation({ mutationFn: () => api.revokeWorker(w.id), onSettled: refresh })
  const remove = useMutation({ mutationFn: () => api.deleteWorker(w.id), onSettled: refresh })
  const error = revoke.error ?? remove.error
  const labels = Object.entries(w.labels ?? {})

  return (
    <tr>
      <td>
        <div className="strong">{w.name}</div>
        <div className="mono muted small">{w.id.slice(0, 8)}</div>
      </td>
      <td>
        <OnlineBadge online={w.online} revoked={w.revoked} />
        {w.unknown.length > 0 && (
          <div className="reason reason-bad">
            running {w.unknown.length} environment{w.unknown.length === 1 ? '' : 's'} the server has no record of
          </div>
        )}
        {error && <div className="reason reason-bad">{error.message}</div>}
      </td>
      <td>
        <Meter used={w.allocated.cpus} total={w.capacity.cpus} label={`${w.allocated.cpus} / ${w.capacity.cpus}`} />
      </td>
      <td>
        <Meter
          used={w.allocated.memory_mib}
          total={w.capacity.memory_mib}
          label={`${formatMemory(w.allocated.memory_mib)} / ${formatMemory(w.capacity.memory_mib)}`}
        />
      </td>
      <td>
        <div className="labels">
          {labels.length === 0 && <span className="muted">none</span>}
          {labels.map(([k, v]) => (
            <span key={k} className="label mono">
              {k}={v}
            </span>
          ))}
        </div>
      </td>
      <td className="muted nowrap">{formatAgo(w.last_seen_at)}</td>
      <td>
        <div className="actions">
          {w.revoked ? (
            <ConfirmButton label="Remove" confirmLabel="Remove?" disabled={remove.isPending} onConfirm={() => remove.mutate()} />
          ) : (
            <ConfirmButton label="Revoke" confirmLabel="Revoke?" disabled={revoke.isPending} onConfirm={() => revoke.mutate()} />
          )}
        </div>
      </td>
    </tr>
  )
}

function Meter({ used, total, label }: { used: number; total: number; label: string }) {
  const pct = total > 0 ? Math.min(100, (used / total) * 100) : 0
  return (
    <div className="meter" title={label}>
      <div className="meter-label nowrap">{label}</div>
      <div className="meter-track">
        <div className={`meter-fill${pct > 90 ? ' meter-hot' : ''}`} style={{ width: `${pct}%` }} />
      </div>
    </div>
  )
}
