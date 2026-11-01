import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from 'react-router'

import { api, type Environment } from './api'
import type { MenuItem } from './code/ContextMenu'
import { desktop } from './desktop'
import { environmentsKey } from './environments'

// canSuspend is whether the environment's worker can suspend it, as the
// worker says.
export const canSuspend = (env: Environment) => env.suspends === true

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

// openVSCodeTab opens the environment in VS Code in the browser, where it
// is called in a click: in a browser, a new tab, opened in the click, which
// a browser allows, and sent on once the editor has its ticket; in the
// desktop app, a tab of its window. It resolves to what went wrong, or ''
// when nothing did.
export function openVSCodeTab(id: string): Promise<string> {
  const w = desktop ? null : window.open('', '_blank')
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

// openVSCode opens the environment in VS Code: in the desktop app, the VS
// Code installed beside it, over SSH; in a browser, VS Code in a new tab.
export function openVSCode(id: string): Promise<string> {
  if (desktop)
    return desktop.openInVSCode(id).then(
      () => '',
      (e: Error) => e.message,
    )
  return openVSCodeTab(id)
}

// vscodeItems are a menu's ways into VS Code: in the desktop app the VS
// Code installed beside it, and VS Code in a tab of the app's window; in a
// browser VS Code in a new tab. onError hears what went wrong, or '' as
// one starts.
export function vscodeItems(id: string, running: boolean, onError: (message: string) => void): MenuItem[] {
  const run = (open: (id: string) => Promise<string>) => () => {
    onError('')
    open(id).then(onError)
  }
  if (!desktop)
    return [{ label: 'Open in VS Code', detail: 'in a new tab', disabled: !running, onSelect: run(openVSCodeTab) }]
  return [
    { label: 'Open in VS Code', detail: 'installed', disabled: !running, onSelect: run(openVSCode) },
    { label: 'VS Code in a Tab', detail: 'in this window', disabled: !running, onSelect: run(openVSCodeTab) },
  ]
}
