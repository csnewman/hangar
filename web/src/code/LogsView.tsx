import { useCallback, useState } from 'react'

import { api, type EnvironmentLogName } from '../api'
import { LogView } from '../components/LogView'
import { logNames } from '../logs'
import { ToolWindow } from './ToolWindow'

// LogsView is the Logs tool window: one of the logs the environment's worker
// keeps of it, from its last start, followed as it grows. It works whatever
// the environment is doing, since the worker, not the machine, keeps them.
// show asks for a log; n changes with each ask.
export function LogsView({
  env,
  show,
  onHide,
}: {
  env: string
  show: { log: EnvironmentLogName; n: number } | null
  onHide: () => void
}) {
  const [chosen, setChosen] = useState<EnvironmentLogName>(show?.log ?? 'console')
  const [asked, setAsked] = useState(show?.n)
  if (show && show.n !== asked) {
    setAsked(show.n)
    setChosen(show.log)
  }
  const load = useCallback((offset?: number) => api.environmentLog(env, chosen, offset), [env, chosen])
  const about = logNames.find((l) => l.name === chosen)?.about

  return (
    <ToolWindow
      title="Logs"
      extra={
        <select
          className="code-logs-pick"
          aria-label="Which log"
          title={about}
          value={chosen}
          onChange={(e) => setChosen(e.target.value as EnvironmentLogName)}
        >
          {logNames.map((l) => (
            <option key={l.name} value={l.name}>
              {l.label}
            </option>
          ))}
        </select>
      }
      onHide={onHide}
    >
      <div className="code-logs">
        <LogView key={`${env}:${chosen}`} load={load} />
      </div>
    </ToolWindow>
  )
}
