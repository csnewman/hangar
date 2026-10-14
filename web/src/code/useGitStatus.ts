import { useCallback, useEffect, useState } from 'react'

import type { CodeClient, GitStatus } from './client'

// useGitStatus is what git says of the folder, reloaded whenever files or
// the repository change, and a reload to call after acting on it.
export function useGitStatus(client: CodeClient): [GitStatus | null, () => void] {
  const [status, setStatus] = useState<GitStatus | null>(null)
  const reload = useCallback(() => {
    client.status().then(setStatus, () => {})
  }, [client])
  useEffect(() => {
    reload()
    let timer: ReturnType<typeof setTimeout> | undefined
    const later = () => {
      clearTimeout(timer)
      timer = setTimeout(reload, 300)
    }
    const offs = [
      client.on('fs.changed', later),
      client.on('git.changed', later),
      client.on('state', (s) => s === 'live' && later()),
    ]
    return () => {
      clearTimeout(timer)
      offs.forEach((off) => off())
    }
  }, [client, reload])
  return [status, reload]
}
