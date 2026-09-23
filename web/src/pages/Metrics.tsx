import { useOutletContext } from 'react-router'

import type { Environment, EnvironmentStats } from '../api'
import { LineChart } from '../components/charts'
import { formatBytes, formatMemory, formatPercent, formatRate } from '../components/format'
import { useEnvironments } from '../environments'
import { useHistory } from '../history'

// MetricsTab is what one environment is using: the latest figures, and how
// they have moved since this page started watching.
export function MetricsTab() {
  const env = useOutletContext<Environment>()
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
          title="Memory"
          current={`${formatMemory(s.memory_used_mib)} of ${formatMemory(s.memory_total_mib)}`}
          times={times}
          max={s.memory_total_mib}
          format={mib}
          series={[{ name: 'Memory', values: pick((x) => x.memory_used_mib) }]}
        />
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
