import './admin.css'

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ScrollText } from 'lucide-react'
import { useCallback } from 'react'
import { Link, useParams } from 'react-router'

import { api, type GPUInfo, type LocalImage, type Worker, type WorkerStats } from '../../api'
import { StatTile } from '../../components/charts'
import { ConfirmButton } from '../../components/ConfirmButton'
import { LogView } from '../../components/LogView'
import { formatAgo, formatBytes, formatCores, formatMemory, formatPercent, shortDigest } from '../../components/format'
import { PhaseBadge, RendererBadge } from '../../components/Status'
import { useEnvironments } from '../../environments'
import { useHistory } from '../../history'
import { Activity } from '../../components/Activity'
import { DetailHead, DetailPage, Meter, NoneChosen, Section } from '../../components/Pane'
import { useEnvironmentTarget } from '../../workspaceLinks'
import { WorkersPane } from './Workers'

// WorkerDetailPage is one worker: the Workers page with this worker chosen.
export function WorkerDetailPage() {
  const { id } = useParams()
  return <WorkersPane id={id} />
}

// WorkerDetail is the chosen worker beside the list: its head, with
// revoking it, then its capacity, machine, environments, image copies, log
// and activity.
export function WorkerDetail({ id }: { id: string }) {
  // Kept polling while hidden, since the tiles chart what the page sees.
  const worker = useQuery({ queryKey: ['workers', id], queryFn: () => api.worker(id), refetchIntervalInBackground: true })
  if (worker.isPending) return null
  if (worker.isError) return <NoneChosen>No such worker.</NoneChosen>
  const w = worker.data
  const labels = Object.entries(w.labels)
  const toLog = () => {
    document.querySelector('[data-section="log"]')?.scrollIntoView({ behavior: 'smooth', block: 'start' })
  }
  return (
    <>
      <DetailHead
        mark={<span className={`phase-dot ${workerTone(w)}`} aria-hidden="true" />}
        title={w.name}
        sub={
          <>
            <span className={w.revoked ? 'pane-pill pane-pill-warn' : w.online ? 'pane-pill pane-pill-good' : 'pane-pill pane-pill-grey'}>
              {w.revoked ? 'revoked' : w.online ? 'online' : 'offline'}
            </span>
            {labels.map(([k, v]) => (
              <span key={k} className="label mono">
                {k}={v}
              </span>
            ))}
            <span>last seen {formatAgo(w.last_seen_at)}</span>
          </>
        }
        actions={
          <>
            <button type="button" className="btn btn-ghost btn-sm" onClick={toLog} disabled={!w.online}>
              <ScrollText size={13} />
              View Log
            </button>
            <WorkerRemoval worker={w} />
          </>
        }
      />
      <DetailPage
        wide
        sections={[
          { id: 'capacity', label: 'Capacity' },
          { id: 'machine', label: 'Machine' },
          { id: 'environments', label: 'Environments' },
          { id: 'images', label: 'Image copies' },
          { id: 'log', label: 'Log' },
          { id: 'activity', label: 'Activity' },
        ]}
      >
        <Section id="capacity" title="Capacity">
          <Capacity worker={w} />
        </Section>
        <Section id="machine" title="Machine">
          <Machine worker={w} />
        </Section>
        <Section id="environments" title="Environments">
          <div className="admin-scroll">
            <Environments worker={w} className="pane-table" />
          </div>
          {w.unknown.length > 0 && <Unknown worker={w} />}
        </Section>
        <Section
          id="images"
          title="Image copies"
          extra={w.images.length > 0 && <span className="muted">{formatBytes(imagesSize(w))} on disk</span>}
        >
          <div className="admin-scroll">
            <Images worker={w} className="pane-table" />
          </div>
        </Section>
        <Section id="log" title="Log">
          <WorkerLog worker={w} />
        </Section>
        <Section id="activity" title="Activity">
          <Activity subjects={[`worker:${w.id}`]} />
        </Section>
      </DetailPage>
    </>
  )
}

// WorkerRemoval revokes a worker, whose credential is then refused, and
// removes a revoked one.
export function WorkerRemoval({ worker: w }: { worker: Worker }) {
  const qc = useQueryClient()
  const refresh = () => qc.invalidateQueries({ queryKey: ['workers'] })
  const revoke = useMutation({ mutationFn: () => api.revokeWorker(w.id), onSettled: refresh })
  const remove = useMutation({ mutationFn: () => api.deleteWorker(w.id), onSettled: refresh })
  const error = revoke.error ?? remove.error
  return (
    <>
      {error && <span className="action-error">{error.message}</span>}
      {w.revoked ? (
        <ConfirmButton label="Remove" confirmLabel="Remove?" disabled={remove.isPending} onConfirm={() => remove.mutate()} />
      ) : (
        <ConfirmButton label="Revoke" confirmLabel="Revoke?" disabled={revoke.isPending} onConfirm={() => revoke.mutate()} />
      )}
    </>
  )
}

// workerTone is the colour of a worker's dot: online, offline or revoked.
function workerTone(w: Worker): string {
  return w.revoked ? 'tone-bad' : w.online ? 'tone-good' : 'tone-idle'
}

function imagesSize(w: Worker): number {
  return w.images.reduce((n, i) => n + i.size_bytes, 0)
}

// useHere is the environments placed on the worker.
function useHere(w: Worker) {
  const envs = useEnvironments()
  return (envs.data ?? []).filter((e) => e.worker_id === w.id)
}

// Capacity is what the worker has given its environments, against what it
// has, and how busy its processors are.
function Capacity({ worker: w }: { worker: Worker }) {
  const here = useHere(w)
  const s = w.stats
  const tile = (label: string, value: string, used: number, total: number, detail: string, colour?: string) => (
    <div className="admin-tile">
      <span className="muted small">{label}</span>
      <strong>{value}</strong>
      <Meter value={total > 0 ? used / total : 0} width={200} colour={colour} />
      <span className="muted small">{detail}</span>
    </div>
  )
  const by = (f: (e: (typeof here)[number]) => string) => here.map(f).join(', ') || 'nothing placed'
  return (
    <div className="admin-tiles">
      {tile(
        'vCPUs allocated',
        `${w.allocated.cpus} of ${w.capacity.cpus}`,
        w.allocated.cpus,
        w.capacity.cpus,
        by((e) => `${e.name} ${e.cpus}`),
      )}
      {tile(
        'Memory allocated',
        `${formatMemory(w.allocated.memory_mib)} of ${formatMemory(w.capacity.memory_mib)}`,
        w.allocated.memory_mib,
        w.capacity.memory_mib,
        by((e) => `${e.name} ${formatMemory(e.memory_mib)}`),
        'var(--series-2)',
      )}
      {s
        ? tile('Host CPU', formatPercent(s.cpu_percent), s.cpu_percent, 100, `load ${s.load1.toFixed(2)}, 1-minute average`)
        : tile('Host CPU', '—', 0, 1, 'not reported yet')}
    </div>
  )
}

// Machine is how busy the worker's machine is, charted as the page sees it,
// and what its virtual GPUs render with.
function Machine({ worker: w }: { worker: Worker }) {
  const history = useHistory<WorkerStats>(`worker:${w.id}`, w.stats)
  const s = w.stats
  const pick = (f: (s: WorkerStats) => number) => history.map((h) => f(h.value))
  return (
    <>
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
            label="Environments' memory"
            value={formatMemory(s.guest_memory_mib)}
            detail={`held, of ${formatMemory(w.allocated.memory_mib)} they reserve`}
            trend={pick((x) => x.guest_memory_mib)}
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
          <StatTile
            label="Support processes"
            value={formatCores(s.support_cpus)}
            detail={s.gpu_cpus > 0.005 ? `${formatCores(s.gpu_cpus)} rendering` : 'GPU, file systems, networks'}
            trend={pick((x) => x.support_cpus)}
          />
        </div>
      )}
      {w.gpu && <GPU gpu={w.gpu} />}
    </>
  )
}

// GPU says what the worker's virtual GPUs render with, as it found when it
// started, and what its configuration asked for.
function GPU({ gpu: g }: { gpu: GPUInfo }) {
  const vulkan = g.vulkan ?? []
  return (
    <div className="panel capacity">
      <div className="capacity-row">
        <span className="muted">Virtual GPU</span>
        <span>
          {g.unavailable ? (
            <span className="reason reason-bad">Not offered: {g.unavailable}</span>
          ) : (
            <>
              <RendererBadge software={!!g.gl?.software} /> <span className="mono">{g.gl?.renderer}</span>
            </>
          )}
        </span>
      </div>
      {!g.unavailable && (
        <div className="capacity-row">
          <span className="muted">Vulkan</span>
          <span className="mono">
            {vulkan.length > 0
              ? vulkan.map((d) => `${d.name}${d.software ? ' (software)' : ''}`).join(', ')
              : g.vulkan_error || 'none'}
          </span>
        </div>
      )}
      <div className="capacity-row">
        <span className="muted">Configured</span>
        <span className="mono">
          renderer: {g.renderer}
          {g.device && `, device: ${g.device}`}
        </span>
      </div>
    </div>
  )
}

// Environments is a table of the environments placed on the worker, each a
// link to its workspace in a tab of its own.
function Environments({ worker: w, className }: { worker: Worker; className: string }) {
  const envs = useEnvironments()
  const here = useHere(w)
  const target = useEnvironmentTarget()
  return (
    <table className={className}>
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
              <Link to={`/environments/${e.id}`} target={target(e.id)} className="strong row-link">
                {e.name}
              </Link>
              <div className="muted small">{e.template}</div>
            </td>
            <td>{e.owner}</td>
            <td>
              <PhaseBadge phase={e.phase} desired={e.desired} workerOnline={e.worker_online} />
              {e.reason && <div className="reason">{e.reason}</div>}
            </td>
            <td className="num nowrap">
              {e.cpus} vCPU · {formatMemory(e.memory_mib)}
            </td>
            <td className="num nowrap">{e.stats ? formatPercent(e.stats.cpu_percent) : '—'}</td>
            <td className="num nowrap">
              {e.stats ? `${formatMemory(e.stats.memory_used_mib)} of ${formatMemory(e.stats.memory_total_mib)}` : '—'}
              {e.stats && <div className="muted small">{formatMemory(e.stats.host_memory_mib)} held</div>}
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
  )
}

// Images is a table of the copies of images in the worker's local store.
function Images({ worker: w, className }: { worker: Worker; className: string }) {
  const envs = useEnvironments()
  const names = new Map((envs.data ?? []).map((e) => [e.id, e.name]))
  return (
    <table className={className}>
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
            key={`${img.ref}@${img.digest}`}
            worker={w}
            image={img}
            pending={w.pending_removals.some((r) => r.ref === img.ref && r.digest === img.digest)}
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
    mutationFn: () => api.removeWorkerImage(w.id, img.ref, img.digest),
    onSettled: () => qc.invalidateQueries({ queryKey: ['workers', w.id] }),
  })
  const inUse = img.environments.length > 0
  const target = useEnvironmentTarget()
  return (
    <tr>
      <td>
        <span className="mono">{img.ref}</span>{' '}
        <Link to={`/admin/audit?subject=${encodeURIComponent(`image:${img.ref}`)}`} className="muted small">
          history
        </Link>
        <div className="muted small mono" title={img.digest}>
          {shortDigest(img.digest)}
          {!img.current && img.state === 'ready' && (
            <>
              {' '}
              <span
                className="badge"
                title="The image has moved on since this copy was taken. Environments made from it keep it; new ones get the current copy."
              >
                older
              </span>
            </>
          )}
        </div>
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
              <Link key={id} to={`/environments/${id}`} target={target(id)} className="label">
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

// Unknown is what the worker runs that the server has no record of: left
// behind by a lost database, or a worker moved between control planes. It is
// never deleted without being asked.
function Unknown({ worker: w }: { worker: Worker }) {
  const qc = useQueryClient()
  const remove = useMutation({
    mutationFn: () => api.removeUnknownEnvironments(w.id, w.unknown),
    onSettled: () => qc.invalidateQueries({ queryKey: ['workers', w.id] }),
  })
  const n = w.unknown.length
  return (
    <div className="notice notice-row">
      <div>
        It is also running {n} environment{n === 1 ? '' : 's'} the server has no record of:{' '}
        <span className="mono">{w.unknown.join(', ')}</span>
        {remove.isSuccess && <div className="muted small">Asked for; they go when the worker next syncs.</div>}
        {remove.error && <div className="action-error">{remove.error.message}</div>}
      </div>
      <ConfirmButton
        label={n === 1 ? 'Delete it' : 'Delete them'}
        confirmLabel="Delete, disks and all?"
        title="Power off and delete, with their disks"
        onConfirm={() => remove.mutate()}
        disabled={remove.isPending}
      />
    </div>
  )
}

// WorkerLog is the latest of what the worker logged, which it keeps in
// memory: why it could not start an environment, among the rest.
function WorkerLog({ worker: w }: { worker: Worker }) {
  const load = useCallback((offset?: number) => api.workerLog(w.id, offset), [w.id])
  return w.online ? (
    <LogView key={w.id} load={load} />
  ) : (
    <p className="muted small">The worker is not connected, so its log cannot be read.</p>
  )
}
