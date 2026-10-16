import { useSearchParams } from 'react-router'

import { PhoneChoice } from '../components/Phone'
import { usePhone } from '../phone'
import { useEnv } from './Environment'
import { MetricsTab } from './Metrics'
import { ProcessesTab } from './Processes'

// PerformanceTab is what an environment is using and what is using it: its
// usage over time, then the processes running in it. A stopped environment
// has neither, and says so once. On a phone they are two views of the one
// screen.
export function PerformanceTab() {
  const env = useEnv()
  const phone = usePhone()
  const [params, setParams] = useSearchParams()
  if (phone && env.phase === 'running') {
    const view = params.get('view') === 'processes' ? 'processes' : 'usage'
    return (
      <>
        <div className="page-pad perf-switch">
          <PhoneChoice
            label="Show"
            value={view}
            options={[
              { value: 'usage', label: 'Usage' },
              { value: 'processes', label: 'Processes' },
            ]}
            onChange={(v) => setParams(v === 'usage' ? {} : { view: v }, { replace: true })}
          />
        </div>
        {view === 'usage' ? <MetricsTab /> : <ProcessesTab />}
      </>
    )
  }
  return (
    <>
      <MetricsTab />
      {env.phase === 'running' && <ProcessesTab />}
    </>
  )
}
