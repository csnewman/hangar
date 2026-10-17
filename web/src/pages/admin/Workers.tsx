import './admin.css'

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { Link, Navigate, NavLink } from 'react-router'

import { api, type Worker } from '../../api'
import { ConfirmButton } from '../../components/ConfirmButton'
import { Meter } from '../../components/charts'
import { formatAgo, formatMemory, formatPercent } from '../../components/format'
import { PageHeader } from '../../components/PageHeader'
import { ListPane, MasterDetail, Meter as PaneMeter, NoneChosen, PaneRoot, SearchField } from '../../components/Pane'
import { OnlineBadge } from '../../components/Status'
import { useEnvironments } from '../../environments'
import { usePhone } from '../../phone'
import { WorkerDetail } from './WorkerDetail'

// WorkersPage is the machines that run environments. On a computer it is a
// list of them beside the one chosen, the first until another is; on a
// phone, a table of them leading to each.
export function WorkersPage() {
  if (!usePhone()) return <WorkersPane />
  return <WorkerTable />
}

// WorkersPane is the workers down the left, each with what it has given its
// environments, and the chosen one, id, beside them.
export function WorkersPane({ id }: { id?: string }) {
  const workers = useQuery({ queryKey: ['workers'], queryFn: api.workers })
  const envs = useEnvironments()
  const [query, setQuery] = useState('')
  const list = (workers.data ?? []).filter((w) => {
    const q = query.trim().toLowerCase()
    return !q || w.name.toLowerCase().includes(q) || Object.entries(w.labels).some(([k, v]) => `${k}=${v}`.includes(q))
  })
  if (!id && workers.data && workers.data.length > 0) {
    return <Navigate to={`/admin/workers/${workers.data[0].id}`} replace />
  }
  const placed = (w: Worker) => (envs.data ?? []).filter((e) => e.worker_id === w.id).length

  return (
    <PaneRoot>
      <MasterDetail
        list={
          <ListPane head={<SearchField value={query} onChange={setQuery} placeholder="Workers or labels" />}>
            {workers.isError && <div className="md-empty-list">Could not load workers: {workers.error.message}</div>}
            {workers.data?.length === 0 && <div className="md-empty-list">No workers have registered.</div>}
            {list.map((w) => {
              const n = placed(w)
              return (
                <NavLink key={w.id} to={`/admin/workers/${w.id}`} className="md-item admin-worker">
                  <span className={`phase-dot ${workerTone(w)}`} aria-hidden="true" />
                  <span className="md-item-main">
                    <span className="admin-worker-head">
                      <span className="md-item-title">{w.name}</span>
                      <span className="md-item-sub">
                        {w.revoked ? 'revoked' : w.online ? 'online' : 'offline'} · {n} env{n === 1 ? '' : 's'}
                      </span>
                    </span>
                    <span className="admin-meters">
                      <span>vCPU</span>
                      <PaneMeter value={w.capacity.cpus > 0 ? w.allocated.cpus / w.capacity.cpus : 0} width={180} />
                      <span>Mem</span>
                      <PaneMeter
                        value={w.capacity.memory_mib > 0 ? w.allocated.memory_mib / w.capacity.memory_mib : 0}
                        width={180}
                        colour="var(--series-2)"
                      />
                    </span>
                    {w.unknown.length > 0 && (
                      <span className="reason reason-bad">
                        {w.unknown.length} environment{w.unknown.length === 1 ? '' : 's'} unknown to the server
                      </span>
                    )}
                  </span>
                </NavLink>
              )
            })}
          </ListPane>
        }
      >
        {id ? (
          <WorkerDetail key={id} id={id} />
        ) : (
          <NoneChosen>{workers.isPending ? 'Loading…' : 'A worker dials in to the control plane once it is started.'}</NoneChosen>
        )}
      </MasterDetail>
    </PaneRoot>
  )
}

// workerTone is the colour of a worker's dot: online, offline or revoked.
function workerTone(w: Worker): string {
  return w.revoked ? 'tone-bad' : w.online ? 'tone-good' : 'tone-idle'
}

function WorkerTable() {
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
      <td data-label="vCPUs">
        <Meter used={w.allocated.cpus} total={w.capacity.cpus} label={`${w.allocated.cpus} / ${w.capacity.cpus}`} />
      </td>
      <td data-label="Memory">
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
      <td className="muted nowrap" data-label="Seen">
        {formatAgo(w.last_seen_at)}
      </td>
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
