import './admin.css'

import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { Navigate, NavLink } from 'react-router'

import { api, type Worker } from '../../api'
import { ListPane, MasterDetail, Meter as PaneMeter, NoneChosen, PaneRoot, SearchField } from '../../components/Pane'
import { useEnvironments } from '../../environments'
import { usePhone } from '../../phone'
import { WorkerDetail } from './WorkerDetail'

// WorkersPage is the machines that run environments: a list of them
// beside the one chosen.
export function WorkersPage() {
  return <WorkersPane />
}

// WorkersPane is the workers down the left, each with what it has given its
// environments, and the chosen one, id, beside them: on a computer the
// first until another is chosen; on a phone the list until one is.
export function WorkersPane({ id }: { id?: string }) {
  const phone = usePhone()
  const workers = useQuery({ queryKey: ['workers'], queryFn: api.workers })
  const envs = useEnvironments()
  const [query, setQuery] = useState('')
  const list = (workers.data ?? []).filter((w) => {
    const q = query.trim().toLowerCase()
    return !q || w.name.toLowerCase().includes(q) || Object.entries(w.labels).some(([k, v]) => `${k}=${v}`.includes(q))
  })
  if (!phone && !id && workers.data && workers.data.length > 0) {
    return <Navigate to={`/admin/workers/${workers.data[0].id}`} replace />
  }
  const placed = (w: Worker) => (envs.data ?? []).filter((e) => e.worker_id === w.id).length

  return (
    <PaneRoot>
      <MasterDetail
        chosen={!!id}
        back={{ to: '/admin/workers', label: 'Workers' }}
        list={
          <ListPane
            title="Workers"
            back={{ to: '/more', label: 'More' }}
            head={<SearchField value={query} onChange={setQuery} placeholder="Workers or labels" />}
          >
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
