import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowDown, X } from 'lucide-react'
import { useMemo, useState } from 'react'

import { api, type ProcessInfo } from '../api'
import { ConfirmButton } from '../components/ConfirmButton'
import { formatAgo, formatBytes } from '../components/format'
import { usePhone } from '../phone'
import { sorts, states, type SortKey } from '../processes'
import { useEnv } from './Environment'

// ProcessesTab is the environment's task manager: what runs in it, what each
// process costs, refreshed every couple of seconds, and a way to stop one.
// On a phone the processes are a list, and one tapped opens a sheet with
// the ways to stop it.
export function ProcessesTab() {
  const phone = usePhone()
  const [open, setOpen] = useState<number | null>(null)
  const env = useEnv()
  const running = env.phase === 'running'
  const list = useQuery({
    queryKey: ['processes', env.id],
    queryFn: () => api.processes(env.id),
    refetchInterval: 2000,
    enabled: running,
  })
  const [sort, setSort] = useState<SortKey>('cpu')
  const [filter, setFilter] = useState('')
  const [kernel, setKernel] = useState(false)

  const shown = useMemo(() => {
    const f = filter.trim().toLowerCase()
    return (list.data?.processes ?? [])
      .filter((p) => kernel || !p.command.startsWith('['))
      .filter(
        (p) => !f || p.command.toLowerCase().includes(f) || p.user.toLowerCase().includes(f) || String(p.pid) === f,
      )
      .sort(sorts[sort])
  }, [list.data, sort, filter, kernel])

  if (!running) {
    return (
      <div className="page-pad">
        <div className="panel empty">The environment is not running.</div>
      </div>
    )
  }
  if (list.isError) {
    return (
      <div className="page-pad">
        <div className="alert">{list.error.message}</div>
      </div>
    )
  }
  const all = list.data?.processes ?? []
  const cpu = all.reduce((s, p) => s + p.cpu_percent, 0)
  const cpus = list.data?.cpus ?? 1
  const memory = all.reduce((s, p) => s + p.rss_bytes, 0)

  if (phone) {
    const chosen = open === null ? undefined : all.find((p) => p.pid === open)
    return (
      <div className="page-pad procs-phone">
        <div className="procs-totals">
          <span>
            <span className="strong">{(cpu / cpus).toFixed(0)}%</span> <span className="muted">CPU of {cpus}</span>
          </span>
          <span>
            <span className="strong">{formatBytes(memory)}</span>{' '}
            <span className="muted">of {formatBytes(list.data?.memory_bytes ?? 0)}</span>
          </span>
          <span className="muted">{all.length} processes</span>
        </div>
        <input
          type="search"
          className="procs-filter"
          placeholder="Filter by command, user or PID"
          aria-label="Filter"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <div className="procs-phone-opts">
          <label className="procs-sort">
            <span>Sort by</span>
            <select value={sort} onChange={(e) => setSort(e.target.value as SortKey)}>
              <option value="cpu">CPU</option>
              <option value="memory">Memory</option>
              <option value="name">Name</option>
              <option value="pid">PID</option>
            </select>
          </label>
          <label className="check">
            <input type="checkbox" checked={kernel} onChange={(e) => setKernel(e.target.checked)} />
            <span>Kernel threads</span>
          </label>
        </div>
        <div className="phone-list">
          {list.isPending && <div className="phone-row phone-row-empty">Reading the process list…</div>}
          {shown.map((p) => (
            <button key={p.pid} type="button" className="phone-row" onClick={() => setOpen(p.pid)}>
              <span className="phone-row-main">
                <span>
                  <span className="strong">{p.name}</span> <span className="muted">· {p.user}</span>
                </span>
                <span className="phone-row-sub mono procs-cmd">{p.command}</span>
              </span>
              <span className="procs-use">
                <span>{p.cpu_percent.toFixed(1)}%</span>
                <span className="phone-row-sub">{formatBytes(p.rss_bytes)}</span>
              </span>
            </button>
          ))}
        </div>
        {chosen && <ProcessSheet env={env.id} p={chosen} onClose={() => setOpen(null)} />}
      </div>
    )
  }

  const header = (key: SortKey, label: string, num = false) => (
    <th className={num ? 'num' : undefined}>
      <button type="button" className={sort === key ? 'sort sort-on' : 'sort'} onClick={() => setSort(key)}>
        {label}
        {sort === key && <ArrowDown size={12} />}
      </button>
    </th>
  )

  return (
    <div className="page-pad">
      <div className="procs-bar">
        <div className="procs-totals">
          <span>
            <span className="strong">{(cpu / cpus).toFixed(0)}%</span> <span className="muted">CPU of {cpus}</span>
          </span>
          <span>
            <span className="strong">{formatBytes(memory)}</span>{' '}
            <span className="muted">resident of {formatBytes(list.data?.memory_bytes ?? 0)}</span>
          </span>
          <span className="muted">{all.length} processes</span>
        </div>
        <label className="check">
          <input type="checkbox" checked={kernel} onChange={(e) => setKernel(e.target.checked)} />
          <span>Kernel threads</span>
        </label>
        <input
          className="procs-filter"
          placeholder="Filter by command, user or PID"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
      </div>
      <div className="panel">
        <table className="table procs">
          <thead>
            <tr>
              {header('name', 'Process')}
              <th>User</th>
              {header('pid', 'PID', true)}
              {header('cpu', 'CPU', true)}
              {header('memory', 'Memory', true)}
              <th className="num">Threads</th>
              <th className="num">Started</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {shown.map((p) => (
              <Row key={p.pid} env={env.id} p={p} cpus={cpus} />
            ))}
            {list.isPending && (
              <tr>
                <td colSpan={8} className="empty">
                  Reading the process list…
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  )
}

function Row({ env, p, cpus }: { env: string; p: ProcessInfo; cpus: number }) {
  const qc = useQueryClient()
  const signal = useMutation({
    mutationFn: (s: 'TERM' | 'KILL') => api.signalProcess(env, p.pid, s),
    onSettled: () => qc.invalidateQueries({ queryKey: ['processes', env] }),
  })
  const protectedProcess = p.protected ?? false
  return (
    <tr>
      <td className="procs-command">
        <div className="strong">{p.name}</div>
        <div className="muted small mono" title={p.command}>
          {p.command}
        </div>
        {signal.error && <div className="reason reason-bad">{signal.error.message}</div>}
      </td>
      <td className="nowrap">{p.user}</td>
      <td className="num mono">{p.pid}</td>
      <td className="num nowrap" title={states[p.state] ?? p.state}>
        <span className="procs-bar-cell">
          <span className="procs-meter" style={{ width: `${Math.min(100, p.cpu_percent / cpus)}%` }} />
          {p.cpu_percent.toFixed(1)}%
        </span>
      </td>
      <td className="num nowrap">{formatBytes(p.rss_bytes)}</td>
      <td className="num">{p.threads}</td>
      <td className="num nowrap muted small">{formatAgo(p.started)}</td>
      <td className="num nowrap">
        {!protectedProcess && (
          <span className="procs-actions">
            <ConfirmButton
              label="End"
              confirmLabel="End it?"
              title="Ask it to stop (TERM)"
              disabled={signal.isPending}
              onConfirm={() => signal.mutate('TERM')}
            />
            <ConfirmButton
              label="Kill"
              confirmLabel="Kill it?"
              title="Stop it without asking (KILL)"
              disabled={signal.isPending}
              onConfirm={() => signal.mutate('KILL')}
            />
          </span>
        )}
      </td>
    </tr>
  )
}

// ProcessSheet is one process, risen over the list: what it is, what it
// costs, and the ways to stop it, each asked twice.
function ProcessSheet({ env, p, onClose }: { env: string; p: ProcessInfo; onClose: () => void }) {
  const qc = useQueryClient()
  const signal = useMutation({
    mutationFn: (s: 'TERM' | 'KILL') => api.signalProcess(env, p.pid, s),
    onSettled: () => qc.invalidateQueries({ queryKey: ['processes', env] }),
    onSuccess: onClose,
  })
  return (
    <div className="phone-sheet-layer">
      <button type="button" className="phone-sheet-backdrop" aria-label="Close" onClick={onClose} />
      <div className="phone-sheet" role="dialog" aria-label={p.name}>
        <div className="phone-sheet-head">
          <h2>{p.name}</h2>
          <button type="button" className="phone-icon-btn" aria-label="Close" onClick={onClose}>
            <X size={20} />
          </button>
        </div>
        <div className="mono procs-sheet-cmd">{p.command}</div>
        <div className="muted small">
          PID {p.pid} · {p.user} · {states[p.state] ?? p.state} · started {formatAgo(p.started)}
        </div>
        <div className="procs-sheet-stats">
          <span>
            <span className="muted small">CPU</span>
            <span className="strong">{p.cpu_percent.toFixed(1)}%</span>
          </span>
          <span>
            <span className="muted small">Memory</span>
            <span className="strong">{formatBytes(p.rss_bytes)}</span>
          </span>
          <span>
            <span className="muted small">Threads</span>
            <span className="strong">{p.threads}</span>
          </span>
        </div>
        {p.protected ? (
          <p className="muted small">The machine's init or Hangar's agent, which cannot be stopped from here.</p>
        ) : (
          <>
            <ConfirmButton
              label="End"
              confirmLabel="End it?"
              title="Ask it to stop (TERM)"
              disabled={signal.isPending}
              onConfirm={() => signal.mutate('TERM')}
            />
            <p className="muted small">Asks it to stop (TERM).</p>
            <ConfirmButton
              label="Kill"
              confirmLabel="Kill it?"
              title="Stop it without asking (KILL)"
              disabled={signal.isPending}
              onConfirm={() => signal.mutate('KILL')}
            />
            <p className="muted small">Stops it without asking (KILL).</p>
          </>
        )}
        {signal.error && <div className="alert">{signal.error.message}</div>}
      </div>
    </div>
  )
}
