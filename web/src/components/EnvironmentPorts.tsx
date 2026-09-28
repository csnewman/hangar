import { useMutation, useQueryClient } from '@tanstack/react-query'

import { api, type Environment } from '../api'
import { environmentsKey } from '../environments'
import { useMe } from '../session'
import { CopyCode } from './CopyCode'

// A DNS label is at most this long, and the name before -env<short> shares
// one with it.
const maxLabel = 63

// EnvironmentPorts says where an environment's own web servers are reached
// through Hangar, and who may reach them.
export function EnvironmentPorts({ env }: { env: Environment }) {
  const me = useMe()
  const qc = useQueryClient()
  const set = useMutation({
    mutationFn: (pub: boolean) => api.setEnvironmentPorts(env.id, pub),
    onSettled: () => qc.invalidateQueries({ queryKey: environmentsKey }),
  })
  const suffix = me.environment_host_suffix
  if (suffix === undefined) return null
  const host = `env${env.short_id}${suffix}`
  // In the prefix style the machine's own name shares the label too.
  const sharing = suffix.startsWith('-') ? suffix.split('.')[0].length : 0
  const longest = maxLabel - `-env${env.short_id}`.length - sharing
  const scheme = window.location.protocol
  const example = `${scheme}//app-${host}`
  const pub = env.ports_public ?? false

  return (
    <section className="panel form section">
      <div className="form-section-head">
        <h2>Web servers</h2>
      </div>
      <p className="muted small">
        Any name of up to {longest} characters and a hyphen before <span className="mono">{host}</span> reaches this
        environment as it is: HTTPS to its port 443, over TLS whatever its certificate, and HTTP to its port 80.
      </p>
      <CopyCode text={example} href={example} />
      <div className="field-row ports-access">
        <label className="check">
          <input type="radio" name={`ports-${env.id}`} checked={!pub} disabled={set.isPending} onChange={() => set.mutate(false)} />
          <span>
            Private
            <small className="muted">Only people who may use this environment, signed in to Hangar.</small>
          </span>
        </label>
        <label className="check">
          <input type="radio" name={`ports-${env.id}`} checked={pub} disabled={set.isPending} onChange={() => set.mutate(true)} />
          <span>
            Public
            <small className="muted">Anyone who can reach Hangar, without signing in.</small>
          </span>
        </label>
      </div>
      {set.error && <div className="alert">{set.error.message}</div>}
    </section>
  )
}
