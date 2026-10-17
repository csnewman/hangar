import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from 'react-router'

import { api, type Environment } from './api'
import { environmentsKey } from './environments'

// useEnvironmentActions is what can be done to an environment, wherever its
// controls are shown: started, stopped, suspended or deleted, then the
// environments read again. A deleted one goes to afterDelete, if given.
export function useEnvironmentActions(env: Environment, afterDelete?: string) {
  const qc = useQueryClient()
  const navigate = useNavigate()
  const refresh = () => qc.invalidateQueries({ queryKey: environmentsKey })
  const start = useMutation({ mutationFn: () => api.startEnvironment(env.id), onSettled: refresh })
  const stop = useMutation({ mutationFn: () => api.stopEnvironment(env.id), onSettled: refresh })
  const suspend = useMutation({ mutationFn: () => api.suspendEnvironment(env.id), onSettled: refresh })
  const remove = useMutation({
    mutationFn: () => api.deleteEnvironment(env.id),
    onSuccess: () => afterDelete && navigate(afterDelete),
    onSettled: refresh,
  })
  return {
    start,
    stop,
    suspend,
    remove,
    busy: start.isPending || stop.isPending || suspend.isPending || remove.isPending,
    error: start.error ?? stop.error ?? suspend.error ?? remove.error,
  }
}

// openInVSCode opens the environment in VS Code in a new browser tab. It is
// called in a click, so the tab is opened then, which a browser allows, and
// sent on once the editor has its ticket. It resolves to what went wrong,
// or '' when nothing did.
export function openInVSCode(id: string): Promise<string> {
  const w = window.open('', '_blank')
  return api.openEditor(id).then(
    (r) => {
      if (w) w.location.href = r.url
      else window.open(r.url, '_blank')
      return ''
    },
    (e: Error) => {
      w?.close()
      return e.message
    },
  )
}
