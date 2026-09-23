import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useParams } from 'react-router'

import { api, type LocalImage, type Worker, type WorkerStats } from '../../api'
import { Meter, StatTile } from '../../components/charts'
import { ConfirmButton } from '../../components/ConfirmButton'
import { formatAgo, formatBytes, formatMemory, formatPercent } from '../../components/format'
import { PageHeader } from '../../components/PageHeader'
import { OnlineBadge, PhaseBadge } from '../../components/Status'
import { useEnvironments } from '../../environments'
import { useHistory } from '../../history'

// WorkerDetailPage is one worker: how loaded its machine is, what it holds,
// and the images in its local store.
export function WorkerDetailPage() {
  const { id } = useParams()
  // Kept polling while hidden, since the tiles chart what the page sees.
  const worker = useQuery({ queryKey: ['workers', id], queryFn: () => api.worker(id!), refetchIntervalInBackground: true })

  if (worker.isPending) return <div className="page" />
  if (worker.isError) {
    return (
      <div className="page">
        <PageHeader crumbs={[{ label: 'Workers', to: '/admin/workers' }]} title="Not found" />
        <div className="panel empty-state">
          No such worker. <Link to="/admin/workers">Back to workers</Link>
        </div>
      </div>
    )
  }
  const w = worker.data
  return (
    <div className="page">
      <PageHeader
        crumbs={[{ label: 'Administration' }, { label: 'Workers', to: '/admin/workers' }]}
        title={w.name}
        subtitle={
          <span className="subtitle-row">
            <OnlineBadge online={w.online} revoked={w.revoked} />
            <span>last seen {formatAgo(w.last_seen_at)}</span>
            <span className="labels">
              {Object.entries(w.labels).map(([k, v]) => (
                <span key={k} className="label mono">
                  {k}={v}
                </span>
              ))}
            </span>
          </span>
        }
      />
      <Machine worker={w} />
      <Environments worker={w} />
      <Images worker={w} />
    </div>
  )
}

function Machine({ worker: w }: { worker: Worker }) {
  const history = useHistory<WorkerStats>(`worker:${w.id}`, w.stats)
  const s = w.stats
  const pick = (f: (s: WorkerStats) => number) => history.map((h) => f(h.value))
  return (
    <section className="section">
      <h2 className="section-title">Machine</h2>
      {!s ? (
        <div className="panel empty-state">This worker has not reported its usage yet.</div>
      ) : (
        <div className="tiles">
          <StatTile label="CPU" value={formatPercent(s.cpu_percent)} trend={pick((x) => x.cpu_percent)} max={100} />
          <StatTile
            label="Memory"
            value={formatMemory(s.memory_used_mib)}
            detail={`of ${formatMemory(s.memory_total_mib)}`}
            trend={pick((x) => x.memory_used_mib)}
            max={s.memory_total_mib}
          />
          <StatTile
            label="Disk"
            value={formatBytes(s.disk_used_bytes)}
            detail={s.disk_total_bytes ? `of ${formatBytes(s.disk_total_bytes)}` : undefined}
            trend={pick((x) => x.disk_used_bytes)}
            max={s.disk_total_bytes || undefined}
          />
          <StatTile label="Load" value={s.load1.toFixed(2)} detail="1-minute average" trend={pick((x) => x.load1)} />
        </div>
      )}
      <div className="panel capacity">
        <div className="capacity-row">
          <span className="muted">Allocated vCPUs</span>
          <Meter used={w.allocated.cpus} total={w.capacity.cpus} label={`${w.allocated.cpus} of ${w.capacity.cpus}`} />
        </div>
        <div className="capacity-row">
          <span className="muted">Allocated memory</span>
          <Meter
            used={w.allocated.memory_mib}
            total={w.capacity.memory_mib}
            label={`${formatMemory(w.allocated.memory_mib)} of ${formatMemory(w.capacity.memory_mib)}`}
          />
        </div>
      </div>
    </section>
  )
}

function Environments({ worker: w }: { worker: Worker }) {
  const envs = useEnvironments()
  const here = (envs.data ?? []).filter((e) => e.worker_id === w.id)
  return (
    <section className="section">
      <h2 className="section-title">
        Environments <span className="section-count">{here.length}</span>
      </h2>
      <div className="panel">
        <table className="table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Owner</th>
              <th>Status</th>
              <th className="num">Size</th>
              <th className="num">CPU</th>
              <th className="num">Memory</th>
            </tr>
          </thead>
          <tbody>
            {here.map((e) => (
              <tr key={e.id}>
                <td>
                  <Link to={`/environments/${e.id}`} className="strong row-link">
                    {e.name}
                  </Link>
                  <div className="muted small">{e.template}</div>
                </td>
                <td>{e.owner}</td>
                <td>
                  <PhaseBadge phase={e.phase} desired={e.desired} />
                  {e.reason && <div className="reason">{e.reason}</div>}
                </td>
                <td className="num nowrap">
                  {e.cpus} vCPU · {formatMemory(e.memory_mib)}
                </td>
                <td className="num nowrap">{e.stats ? formatPercent(e.stats.cpu_percent) : '—'}</td>
                <td className="num nowrap">
                  {e.stats ? `${formatMemory(e.stats.memory_used_mib)} of ${formatMemory(e.stats.memory_total_mib)}` : '—'}
                </td>
              </tr>
            ))}
            {envs.isSuccess && here.length === 0 && (
              <tr>
                <td colSpan={6} className="empty">
                  Nothing is placed on this worker.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
      {w.unknown.length > 0 && (
        <div className="notice">
          It is also running {w.unknown.length} environment{w.unknown.length === 1 ? '' : 's'} the server has no
          record of: <span className="mono">{w.unknown.join(', ')}</span>
        </div>
      )}
    </section>
  )
}

function Images({ worker: w }: { worker: Worker }) {
  const envs = useEnvironments()
  const names = new Map((envs.data ?? []).map((e) => [e.id, e.name]))
  const total = w.images.reduce((n, i) => n + i.size_bytes, 0)
  return (
    <section className="section">
      <h2 className="section-title">
        Local images <span className="section-count">{w.images.length}</span>
        {w.images.length > 0 && <span className="muted small">{formatBytes(total)} on disk</span>}
      </h2>
      <div className="panel">
        <table className="table">
          <thead>
            <tr>
              <th>Image</th>
              <th className="num">Size</th>
              <th>Used by</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {w.images.map((img) => (
              <ImageRow
                key={img.ref}
                worker={w}
                image={img}
                pending={w.pending_removals.includes(img.ref)}
                names={names}
              />
            ))}
            {w.images.length === 0 && (
              <tr>
                <td colSpan={4} className="empty">
                  The worker holds no images. It fetches one when an environment first needs it.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </section>
  )
}

function ImageRow({
  worker: w,
  image: img,
  pending,
  names,
}: {
  worker: Worker
  image: LocalImage
  pending: boolean
  names: Map<string, string>
}) {
  const qc = useQueryClient()
  const remove = useMutation({
    mutationFn: () => api.removeWorkerImage(w.id, img.ref),
    onSettled: () => qc.invalidateQueries({ queryKey: ['workers', w.id] }),
  })
  const inUse = img.environments.length > 0
  return (
    <tr>
      <td>
        <span className="mono">{img.ref}</span>
        {img.state === 'fetching' && (
          <div className="reason">
            <span className="badge badge-busy">
              <span className="dot" />
              fetching
            </span>
          </div>
        )}
        {pending && (
          <div className="reason">
            <span className="badge badge-busy">
              <span className="dot" />
              removing
            </span>
          </div>
        )}
        {remove.error && <div className="reason reason-bad">{remove.error.message}</div>}
      </td>
      <td className="num nowrap">{formatBytes(img.size_bytes)}</td>
      <td>
        {inUse ? (
          <div className="labels">
            {img.environments.map((id) => (
              <Link key={id} to={`/environments/${id}`} className="label">
                {names.get(id) ?? id.slice(0, 8)}
              </Link>
            ))}
          </div>
        ) : (
          <span className="muted">nothing</span>
        )}
      </td>
      <td>
        <div className="actions">
          <ConfirmButton
            label="Delete"
            confirmLabel="Delete?"
            disabled={inUse || pending || remove.isPending || img.state !== 'ready'}
            title={inUse ? 'In use by environments on this worker: delete them first' : undefined}
            onConfirm={() => remove.mutate()}
          />
        </div>
      </td>
    </tr>
  )
}
