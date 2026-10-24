import type { EnvironmentStats } from '../api'
import { LineChart } from '../components/charts'
import { formatBytes, formatCores, formatMemory, formatPercent, formatRate } from '../components/format'
import { useEnvironments } from '../environments'
import { useHistory } from '../history'
import { useEnv } from './Environment'

// MetricsTab is what one environment is using: the latest figures, and how
// they have moved since this page started watching.
export function MetricsTab() {
  const env = useEnv()
  useEnvironments({ watching: true })
  const history = useHistory<EnvironmentStats>(`env:${env.id}`, env.stats)
  const s = env.stats

  if (!s) {
    return (
      <div className="page-pad">
        <div className="panel empty-state">
          {env.phase === 'running'
            ? 'Waiting for the first measurement from its worker…'
            : 'Usage is measured while the environment is running.'}
        </div>
      </div>
    )
  }

  const times = history.map((h) => h.at)
  const pick = (f: (s: EnvironmentStats) => number) => history.map((h) => f(h.value))
  const mib = (v: number) => formatMemory(Math.round(v))

  return (
    <div className="page-pad metrics">
      <div className="metrics-note muted">
        <span>
          Disk used <span className="strong">{formatBytes(s.disk_used_bytes)}</span> by the writable layer and
          Docker store
          {s.swap_total_mib > 0 && (
            <>
              , <span className="strong">{formatBytes(s.swap_disk_bytes)}</span> by its swap
            </>
          )}
        </span>
        <span>Charts show what this page has seen, a sample every five seconds.</span>
      </div>
      <div className="charts">
        <LineChart
          title="CPU"
          current={`${formatPercent(s.cpu_percent)} of ${env.cpus} vCPU${env.cpus === 1 ? '' : 's'}`}
          times={times}
          max={100}
          format={formatPercent}
          series={[{ name: 'CPU', values: pick((x) => x.cpu_percent) }]}
        />
        <LineChart
          title="Support processes"
          current={`${formatCores(s.support_cpus)} of the worker's CPU${s.gpu_cpus > 0.005 ? `, ${formatCores(s.gpu_cpus)} rendering` : ''}`}
          times={times}
          format={formatCores}
          floor={0.1}
          series={[
            { name: 'All', values: pick((x) => x.support_cpus) },
            { name: 'GPU', values: pick((x) => x.gpu_cpus) },
          ]}
        />
        <LineChart
          title="Memory"
          current={`${formatMemory(s.memory_used_mib)} used, ${formatMemory(s.memory_cached_mib)} cache, of ${formatMemory(s.memory_total_mib)} · ${formatMemory(s.host_memory_mib)} on its worker`}
          times={times}
          max={s.memory_total_mib}
          format={mib}
          series={[
            { name: 'Used', values: pick((x) => x.memory_used_mib) },
            { name: 'With cache', values: pick((x) => x.memory_used_mib + x.memory_cached_mib) },
            { name: 'On its worker', values: pick((x) => x.host_memory_mib) },
          ]}
        />
        {s.swap_total_mib > 0 && (
          <LineChart
            title="Swap"
            current={`${formatMemory(s.swap_used_mib)} swapped out, of ${formatMemory(s.swap_total_mib)} · ${formatMemory(s.swap_compressed_mib)} compressed into ${formatMemory(s.swap_pool_mib)} of its memory, ${formatMemory(Math.max(s.swap_used_mib - s.swap_compressed_mib, 0))} on disk`}
            times={times}
            format={mib}
            floor={64}
            series={[
              { name: 'Swapped out', values: pick((x) => x.swap_used_mib) },
              { name: 'On disk', values: pick((x) => Math.max(x.swap_used_mib - x.swap_compressed_mib, 0)) },
              { name: 'Memory it takes', values: pick((x) => x.swap_pool_mib) },
            ]}
          />
        )}
        {s.swap_total_mib > 0 && (
          <LineChart
            title="Swapping"
            current={`${formatRate(s.swap_in_bps)} in · ${formatRate(s.swap_out_bps)} out`}
            times={times}
            format={formatRate}
            floor={1000}
            series={[
              { name: 'In', values: pick((x) => x.swap_in_bps) },
              { name: 'Out', values: pick((x) => x.swap_out_bps) },
            ]}
          />
        )}
        <LineChart
          title="Disk I/O"
          current={`${formatRate(s.disk_read_bps)} read · ${formatRate(s.disk_write_bps)} write`}
          times={times}
          format={formatRate}
          floor={1000}
          series={[
            { name: 'Read', values: pick((x) => x.disk_read_bps) },
            { name: 'Write', values: pick((x) => x.disk_write_bps) },
          ]}
        />
        <LineChart
          title="Network"
          current={`${formatRate(s.net_rx_bps)} in · ${formatRate(s.net_tx_bps)} out`}
          times={times}
          format={formatRate}
          floor={1000}
          series={[
            { name: 'In', values: pick((x) => x.net_rx_bps) },
            { name: 'Out', values: pick((x) => x.net_tx_bps) },
          ]}
        />
      </div>
    </div>
  )
}
