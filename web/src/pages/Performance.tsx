import { useOutletContext } from 'react-router'

import type { Environment } from '../api'
import { MetricsTab } from './Metrics'
import { ProcessesTab } from './Processes'

// PerformanceTab is what an environment is using and what is using it: its
// usage over time, then the processes running in it. A stopped environment
// has neither, and says so once.
export function PerformanceTab() {
  const env = useOutletContext<Environment>()
  return (
    <>
      <MetricsTab />
      {env.phase === 'running' && <ProcessesTab />}
    </>
  )
}
