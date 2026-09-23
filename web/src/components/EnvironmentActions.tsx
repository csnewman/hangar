import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Play, Square } from 'lucide-react'
import { useNavigate } from 'react-router'

import { api, type Environment } from '../api'
import { environmentsKey } from '../environments'
import { ConfirmButton } from './ConfirmButton'

// EnvironmentActions are the controls for one environment, the same wherever
// it is shown.
export function EnvironmentActions({ env, afterDelete }: { env: Environment; afterDelete?: string }) {
  const qc = useQueryClient()
  const navigate = useNavigate()
  const refresh = () => qc.invalidateQueries({ queryKey: environmentsKey })
  const start = useMutation({ mutationFn: () => api.startEnvironment(env.id), onSettled: refresh })
  const stop = useMutation({ mutationFn: () => api.stopEnvironment(env.id), onSettled: refresh })
  const remove = useMutation({
    mutationFn: () => api.deleteEnvironment(env.id),
    onSuccess: () => afterDelete && navigate(afterDelete),
    onSettled: refresh,
  })
  const busy = start.isPending || stop.isPending || remove.isPending
  const deleting = env.desired === 'deleted'
  const error = start.error ?? stop.error ?? remove.error

  return (
    <div className="actions">
      {error && <span className="action-error">{error.message}</span>}
      {env.desired === 'running' ? (
        <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => stop.mutate()}>
          <Square size={13} />
          Stop
        </button>
      ) : (
        <button type="button" className="btn btn-ghost" disabled={busy || deleting} onClick={() => start.mutate()}>
          <Play size={13} />
          Start
        </button>
      )}
      <ConfirmButton label="Delete" confirmLabel="Delete?" disabled={busy || deleting} onConfirm={() => remove.mutate()} />
    </div>
  )
}
