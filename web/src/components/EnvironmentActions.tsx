import { Pause, Play, Square } from 'lucide-react'

import type { Environment } from '../api'
import { useEnvironmentActions } from '../environmentActions'
import { ConfirmButton } from './ConfirmButton'

// EnvironmentActions are the controls for one environment, the same wherever
// it is shown.
export function EnvironmentActions({ env, afterDelete }: { env: Environment; afterDelete?: string }) {
  const { start, stop, suspend, remove, busy, error } = useEnvironmentActions(env, afterDelete)
  const deleting = env.desired === 'deleted'

  return (
    <div className="actions">
      {error && <span className="action-error">{error.message}</span>}
      {env.desired === 'running' ? (
        <>
          <button
            type="button"
            className="btn btn-ghost"
            disabled={busy || env.phase !== 'running'}
            title="Keep its memory on disk and stop it; starting it again resumes where it was"
            onClick={() => suspend.mutate()}
          >
            <Pause size={13} />
            Suspend
          </button>
          <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => stop.mutate()}>
            <Square size={13} />
            Stop
          </button>
        </>
      ) : (
        <>
          <button type="button" className="btn btn-ghost" disabled={busy || deleting} onClick={() => start.mutate()}>
            <Play size={13} />
            {env.phase === 'suspended' ? 'Resume' : 'Start'}
          </button>
          {env.desired === 'suspended' && (
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy}
              title="Discard its memory; its disks stay as they are"
              onClick={() => stop.mutate()}
            >
              <Square size={13} />
              Stop
            </button>
          )}
        </>
      )}
      <ConfirmButton label="Delete" confirmLabel="Delete?" disabled={busy || deleting} onConfirm={() => remove.mutate()} />
    </div>
  )
}
