import { useCallback } from 'react'
import { useSearchParams } from 'react-router'

import { api, type EnvironmentLogName } from '../api'
import { LogView } from '../components/LogView'
import { usePhone } from '../phone'
import { useEnv } from './Environment'

const logNames: { name: EnvironmentLogName; label: string; about: string }[] = [
  {
    name: 'console',
    label: 'Console',
    about: "The machine's serial console: the kernel, Hangar's init assembling the root, and systemd.",
  },
  { name: 'worker', label: 'Worker', about: 'What its worker logged about it: each step of starting, and any error.' },
  { name: 'monitor', label: 'Monitor', about: "Cloud Hypervisor's own log, the virtual machine's monitor." },
  { name: 'fs', label: 'Image', about: 'The backend serving its image to the machine over virtio-fs.' },
  { name: 'gpu', label: 'GPU', about: "The virtual GPU's backend, for an environment with one." },
]

// LogsTab shows the logs an environment's worker keeps of it, from its last
// start, which say why a start is stuck or failed. On a phone the logs are
// chips above the log, which wraps its lines.
export function LogsTab() {
  const env = useEnv()
  const [params, setParams] = useSearchParams()
  const chosen = logNames.find((l) => l.name === params.get('log')) ?? logNames[0]
  const load = useCallback(
    (offset?: number) => api.environmentLog(env.id, chosen.name, offset),
    [env.id, chosen.name],
  )
  const phone = usePhone()
  const [seg, segOn] = phone ? ['phone-pill', 'phone-pill phone-pill-on'] : ['seg', 'seg seg-on']

  return (
    <div className="page-pad logs-tab">
      <div className={phone ? 'phone-pills' : 'segmented'}>
        {logNames.map((l) => (
          <button
            key={l.name}
            type="button"
            className={l.name === chosen.name ? segOn : seg}
            aria-pressed={l.name === chosen.name}
            onClick={() => setParams({ log: l.name }, { replace: true })}
          >
            {l.label}
          </button>
        ))}
      </div>
      <p className="muted small">{chosen.about}</p>
      <LogView key={`${env.id}:${chosen.name}`} load={load} />
    </div>
  )
}
