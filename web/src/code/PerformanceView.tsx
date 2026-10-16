import { useQuery, useQueryClient } from '@tanstack/react-query'
import { ArrowDown, ExternalLink, ListTree } from 'lucide-react'
import { useMemo, useState, type MouseEvent, type ReactNode } from 'react'
import { Link } from 'react-router'

import { api, type Environment, type EnvironmentStats, type ProcessInfo } from '../api'
import { Sparkline } from '../components/charts'
import { ConfirmButton } from '../components/ConfirmButton'
import { formatAgo, formatBytes, formatMemory, formatPercent, formatRate } from '../components/format'
import { useHistory } from '../history'
import { sorts, states, type SortKey } from '../processes'
import { copyText, useContextMenu, type MenuItem } from './ContextMenu'
import { ToolWindow } from './ToolWindow'

type Shown = { p: ProcessInfo; depth: number }

// PerformanceView is the Code tab's Performance tool window: what the
// environment is using, then the processes using it, as a list or as the
// tree of which started which, and the one chosen in detail with the ways
// to stop it. It reads the processes only while it is shown (active).
export function PerformanceView({ env, active, onHide }: { env: Environment; active: boolean; onHide: () => void }) {
  const qc = useQueryClient()
  const list = useQuery({
    queryKey: ['processes', env.id],
    queryFn: () => api.processes(env.id),
    refetchInterval: 2000,
    enabled: active && env.phase === 'running',
  })
  const [sort, setSort] = useState<SortKey>('cpu')
  const [filter, setFilter] = useState('')
  const [kernel, setKernel] = useState(false)
  const [tree, setTree] = useState(false)
  const [selected, setSelected] = useState<number | null>(null)
  const [error, setError] = useState('')
  const menu = useContextMenu()

  const all = useMemo(() => list.data?.processes ?? [], [list.data])
  const shown = useMemo(() => arrange(all, { sort, filter, kernel, tree }), [all, sort, filter, kernel, tree])
  const chosen = selected === null ? undefined : all.find((p) => p.pid === selected)

  const signal = (p: ProcessInfo, s: 'TERM' | 'KILL') => {
    setError('')
    api.signalProcess(env.id, p.pid, s).then(
      () => qc.invalidateQueries({ queryKey: ['processes', env.id] }),
      (e: Error) => setError(e.message),
    )
  }

  const views: MenuItem[] = [
    { label: 'Show as a Tree', checked: tree, onSelect: () => setTree(!tree) },
    { label: 'Show Kernel Threads', checked: kernel, onSelect: () => setKernel(!kernel) },
  ]
  const processMenu = (p: ProcessInfo): MenuItem[] => [
    ...(p.protected
      ? []
      : ([
          { label: 'End', detail: 'TERM', confirm: `End ${p.name}?`, onSelect: () => signal(p, 'TERM') },
          {
            label: 'Kill',
            detail: 'KILL',
            danger: true,
            confirm: `Kill ${p.name}?`,
            onSelect: () => signal(p, 'KILL'),
          },
          'separator',
        ] as MenuItem[])),
    { label: 'Copy PID', onSelect: () => copyText(String(p.pid)) },
    { label: 'Copy Command', onSelect: () => copyText(p.command) },
    'separator',
    ...views,
  ]

  const header = (key: SortKey, label: string, className: string) => (
    <button
      type="button"
      className={sort === key ? `code-perf-sort code-perf-sort-on ${className}` : `code-perf-sort ${className}`}
      onClick={() => setSort(key)}
    >
      {label}
      {sort === key && <ArrowDown size={11} />}
    </button>
  )

  return (
    <ToolWindow
      title="Performance"
      onHide={onHide}
      extra={list.data && <span className="muted small">{`${shown.length} processes`}</span>}
      actions={
        <>
          <button
            type="button"
            className={tree ? 'code-tool-btn code-tool-btn-on' : 'code-tool-btn'}
            title="Show as a tree"
            aria-pressed={tree}
            onClick={() => setTree(!tree)}
          >
            <ListTree size={13} />
          </button>
          <Link className="code-tool-btn" title="Open the Performance page" to={`/environments/${env.id}/performance`}>
            <ExternalLink size={13} />
          </Link>
        </>
      }
    >
      <div className="code-perf">
        <Usage env={env} />
        <input
          type="search"
          className="code-log-filter"
          placeholder="Filter by command, user or PID"
          aria-label="Filter processes"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <div className="code-perf-head">
          {header('name', 'Process', 'code-perf-name')}
          {header('cpu', 'CPU', 'code-perf-num')}
          {header('memory', 'Memory', 'code-perf-mem')}
        </div>
        <div
          className="code-perf-list"
          onContextMenu={(e) => {
            if (e.target === e.currentTarget) menu.open(e, views)
          }}
        >
          {error && <div className="code-note code-note-error">{error}</div>}
          {list.isError && <div className="code-note code-note-error">{list.error.message}</div>}
          {list.isPending && <div className="code-note">Reading the process list…</div>}
          {list.data && shown.length === 0 && <div className="code-note">No process matches.</div>}
          {shown.map(({ p, depth }) => (
            <div
              key={p.pid}
              className={p.pid === selected ? 'code-perf-row code-perf-row-on' : 'code-perf-row'}
              style={{ paddingLeft: 8 + depth * 12 }}
              title={p.command}
              onClick={() => setSelected(p.pid)}
              onContextMenu={(e: MouseEvent) => {
                setSelected(p.pid)
                menu.open(e, processMenu(p))
              }}
            >
              <span className="code-perf-name">
                <span className="code-perf-proc">{p.name}</span>
                <span className="code-perf-cmd">{p.command}</span>
              </span>
              <span className="code-perf-num">{p.cpu_percent.toFixed(1)}%</span>
              <span className="code-perf-mem">{formatBytes(p.rss_bytes)}</span>
            </div>
          ))}
        </div>
        {chosen && (
          <ProcessDetails
            key={chosen.pid}
            p={chosen}
            parent={all.find((x) => x.pid === chosen.ppid)}
            onParent={setSelected}
            onSignal={(s) => signal(chosen, s)}
          />
        )}
      </div>
      {menu.menu}
    </ToolWindow>
  )
}

// arrange is the processes to show, in order: those the filter matches,
// sorted, or in a tree, each under the process that started it and with
// what it started sorted below it. A tree filtered keeps the processes
// above each match, so a match is still where it runs.
function arrange(
  all: ProcessInfo[],
  { sort, filter, kernel, tree }: { sort: SortKey; filter: string; kernel: boolean; tree: boolean },
): Shown[] {
  const f = filter.trim().toLowerCase()
  const listed = all.filter((p) => kernel || !p.command.startsWith('['))
  const matches = (p: ProcessInfo) =>
    !f || p.command.toLowerCase().includes(f) || p.user.toLowerCase().includes(f) || String(p.pid) === f
  if (!tree)
    return listed
      .filter(matches)
      .sort(sorts[sort])
      .map((p) => ({ p, depth: 0 }))

  const byPid = new Map(listed.map((p) => [p.pid, p]))
  const keep = new Set<number>()
  for (const p of listed) {
    if (!matches(p)) continue
    for (let q: ProcessInfo | undefined = p; q && !keep.has(q.pid); q = byPid.get(q.ppid)) keep.add(q.pid)
  }
  const children = new Map<number, ProcessInfo[]>()
  const roots: ProcessInfo[] = []
  for (const p of listed) {
    if (!keep.has(p.pid)) continue
    if (byPid.has(p.ppid) && p.ppid !== p.pid) {
      const c = children.get(p.ppid) ?? []
      c.push(p)
      children.set(p.ppid, c)
    } else roots.push(p)
  }
  const out: Shown[] = []
  const walk = (ps: ProcessInfo[], depth: number) => {
    for (const p of ps.sort(sorts[sort])) {
      out.push({ p, depth })
      walk(children.get(p.pid) ?? [], depth + 1)
    }
  }
  walk(roots, 0)
  return out
}

// Usage is what the environment is using, as its worker last measured it,
// each with how it has moved while this page has been watching.
function Usage({ env }: { env: Environment }) {
  const history = useHistory<EnvironmentStats>(`env:${env.id}`, env.stats)
  const s = env.stats
  if (!s) return <div className="code-note">Waiting for the first measurement from its worker…</div>
  const pick = (f: (s: EnvironmentStats) => number) => history.map((h) => f(h.value))
  return (
    <div className="code-perf-usage">
      <Meter
        label="CPU"
        value={`${formatPercent(s.cpu_percent)} of ${env.cpus}`}
        trend={pick((x) => x.cpu_percent)}
        max={100}
      />
      <Meter
        label="Memory"
        value={`${formatMemory(s.memory_used_mib)} of ${formatMemory(s.memory_total_mib)}`}
        trend={pick((x) => x.memory_used_mib)}
        max={s.memory_total_mib}
      />
      <Meter
        label="Disk"
        value={`${formatRate(s.disk_read_bps)} in · ${formatRate(s.disk_write_bps)} out`}
        trend={pick((x) => x.disk_read_bps + x.disk_write_bps)}
      />
      <Meter
        label="Network"
        value={`${formatRate(s.net_rx_bps)} in · ${formatRate(s.net_tx_bps)} out`}
        trend={pick((x) => x.net_rx_bps + x.net_tx_bps)}
      />
    </div>
  )
}

function Meter({ label, value, trend, max }: { label: string; value: string; trend: number[]; max?: number }) {
  return (
    <>
      <span className="code-perf-label">{label}</span>
      <span className="code-perf-value" title={value}>
        {value}
      </span>
      <span className="code-perf-trend">{trend.length > 1 && <Sparkline values={trend} max={max} />}</span>
    </>
  )
}

// Detail is one line of the details pane: what, and its value.
function Detail({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <span className="code-detail-label">{label}</span>
      <span className="code-detail-value">{children}</span>
    </>
  )
}

function ProcessDetails({
  p,
  parent,
  onParent,
  onSignal,
}: {
  p: ProcessInfo
  parent?: ProcessInfo
  onParent: (pid: number) => void
  onSignal: (s: 'TERM' | 'KILL') => void
}) {
  return (
    <div className="code-details">
      <div className="code-details-head">
        <span className="strong">{p.name}</span>
        <span className="muted small">{states[p.state] ?? p.state}</span>
      </div>
      <div className="code-detail-grid">
        <Detail label="PID">
          <button type="button" className="code-link" title="Copy it" onClick={() => copyText(String(p.pid))}>
            {p.pid}
          </button>
        </Detail>
        <Detail label="Started by">
          {parent ? (
            <button type="button" className="code-link" title="Show it" onClick={() => onParent(parent.pid)}>
              {parent.name} ({parent.pid})
            </button>
          ) : (
            p.ppid
          )}
        </Detail>
        <Detail label="User">{p.user}</Detail>
        <Detail label="Command">
          <button
            type="button"
            className="code-link code-perf-command"
            title="Copy it"
            onClick={() => copyText(p.command)}
          >
            {p.command}
          </button>
        </Detail>
        <Detail label="CPU">{p.cpu_percent.toFixed(1)}%</Detail>
        <Detail label="Memory">{formatBytes(p.rss_bytes)}</Detail>
        <Detail label="Threads">{p.threads}</Detail>
        <Detail label="Started">{formatAgo(p.started)}</Detail>
      </div>
      {p.protected ? (
        <p className="muted small code-perf-protected">
          The machine's init or Hangar's agent, which cannot be stopped from here.
        </p>
      ) : (
        <div className="code-details-actions">
          <ConfirmButton
            label="End"
            confirmLabel="End it?"
            title="Ask it to stop (TERM)"
            onConfirm={() => onSignal('TERM')}
          />
          <ConfirmButton
            label="Kill"
            confirmLabel="Kill it?"
            title="Stop it without asking (KILL)"
            onConfirm={() => onSignal('KILL')}
          />
        </div>
      )}
    </div>
  )
}
