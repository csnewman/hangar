import { useEffect, useState } from 'react'

import type { CodeClient } from './client'

// How often the way to the environment is timed.
const every = 5000

// useLatency is how long a request takes to reach the environment's editor
// service and come back, timed every few seconds while the connection is
// live and the page is shown; null until timed, and while not live.
export function useLatency(client: CodeClient, live: boolean): number | null {
  const [ms, setMs] = useState<number | null>(null)
  useEffect(() => {
    if (!live) return
    let stopped = false
    let timer: ReturnType<typeof setTimeout> | undefined
    const time = async () => {
      if (!document.hidden) {
        const took = await client.ping().catch(() => null)
        if (stopped) return
        setMs(took)
      }
      timer = setTimeout(time, every)
    }
    time()
    return () => {
      stopped = true
      clearTimeout(timer)
    }
  }, [client, live])
  return live ? ms : null
}
