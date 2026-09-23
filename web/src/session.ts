import { createContext, useContext } from 'react'

import type { Me } from './api'

export const meKey = ['me'] as const

export const MeContext = createContext<Me | null>(null)

// useMe is the signed-in user. It may only be called beneath RequireAuth,
// which does not render its children until there is one.
export function useMe(): Me {
  const me = useContext(MeContext)
  if (!me) throw new Error('useMe outside RequireAuth')
  return me
}

// Tabs of one browser share a session cookie, so signing in or out in one
// changes every other. They tell each other over this channel; a tab that
// hears it rechecks who it is signed in as.
const channelName = 'hangar.session'

export function announceSessionChange() {
  try {
    const ch = new BroadcastChannel(channelName)
    ch.postMessage('changed')
    ch.close()
  } catch {
    // Without BroadcastChannel other tabs catch up when they next get focus.
  }
}

export function onSessionChange(fn: () => void): () => void {
  try {
    const ch = new BroadcastChannel(channelName)
    ch.onmessage = fn
    return () => ch.close()
  } catch {
    return () => {}
  }
}
