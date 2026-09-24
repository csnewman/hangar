import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from 'react-router'

import { api, type Worker } from '../../api'
import { ConfirmButton } from '../../components/ConfirmButton'
import { Meter } from '../../components/charts'
import { formatAgo, formatMemory, formatPercent } from '../../components/format'
import { PageHeader } from '../../components/PageHeader'
import { OnlineBadge } from '../../components/Status'

export function WorkersPage() {
  const workers = useQuery({ queryKey: ['workers'], queryFn: api.workers })

  return (
    <div className="page">
      <PageHeader
        crumbs={[{ label: 'Administration' }]}
        title="Workers"
        subtitle="Machines that run environments. Each dials in to the control plane."
      />

      {workers.isError && <div className="alert">Could not load workers: {workers.error.message}</div>}

      <div className="panel">
        <table className="table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Status</th>
              <th>Allocated vCPUs</th>
              <th>Allocated memory</th>
              <th className="num">Usage</th>
              <th>Labels</th>
              <th>Last seen</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {workers.data?.map((w) => <WorkerRow key={w.id} worker={w} />)}
            {workers.data?.length === 0 && (
              <tr>
                <td colSpan={8} className="empty">
                  No workers have registered.
                </td>
              </tr>
            )}
            {workers.isPending && (
              <tr>
                <td colSpan={8} className="empty">
                  Loading…
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
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
        <Link to={`/admin/workers/${w.id}`} className="strong row-link">
          {w.name}
        </Link>
        <div className="mono muted small">{w.id.slice(0, 8)}</div>
      </td>
      <td>
        <OnlineBadge online={w.online} revoked={w.revoked} />
        {w.unknown.length > 0 && (
          <Link to={`/admin/workers/${w.id}`} className="reason reason-bad">
            running {w.unknown.length} environment{w.unknown.length === 1 ? '' : 's'} the server has no record of
          </Link>
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
      <td className="num nowrap">
        {w.stats ? (
          <>
            <div>{formatPercent(w.stats.cpu_percent)} CPU</div>
            <div className="muted small">
              {formatMemory(w.stats.memory_used_mib)} of {formatMemory(w.stats.memory_total_mib)}
            </div>
          </>
        ) : (
          <span className="muted">—</span>
        )}
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
