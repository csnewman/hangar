// What the app knows of each server between looks: whether it is signed
// in, as whom, and its environments. The menu bar, the switcher, the home
// page and notifications all read from here, so a server is asked once
// every few seconds however many parts of the app show it.

import { EventEmitter } from 'node:events'

import * as api from './api'
import type { Environment, Me } from './api'
import * as store from './store'
import type { Server } from './store'

export interface ServerState {
  server: Server
  // compatibility is the version handshake's answer, once asked.
  compatibility?: api.Compatibility
  signedIn: boolean
  me?: Me
  environments: Environment[]
  error?: string
}

// How often a server is asked: often enough for a start's progress to move.
const every = 4000

class Watch extends EventEmitter {
  states = new Map<string, ServerState>()
  private timer: NodeJS.Timeout | undefined

  state(s: Server): ServerState {
    let st = this.states.get(s.id)
    if (!st) {
      st = { server: s, signedIn: false, environments: [] }
      this.states.set(s.id, st)
    }
    st.server = s
    return st
  }

  all(): ServerState[] {
    return store.servers().map((s) => this.state(s))
  }

  start() {
    const tick = async () => {
      await this.refresh()
      this.timer = setTimeout(tick, every)
    }
    tick()
  }

  // refresh asks every server at once, without waiting for the next look.
  async refresh() {
    for (const id of this.states.keys()) {
      if (!store.server(id)) this.states.delete(id)
    }
    await Promise.all(store.servers().map((s) => this.look(s)))
    this.emit('change')
  }

  private async look(s: Server) {
    const st = this.state(s)
    if (!st.compatibility?.ok) {
      st.compatibility = await api.checkServer(s.url)
      if (!st.compatibility.ok) {
        st.signedIn = false
        st.environments = []
        return
      }
    }
    try {
      const [me, envs] = await Promise.all([api.me(s), api.environments(s)])
      const before = new Map(st.environments.map((e) => [e.id, e]))
      st.signedIn = true
      st.me = me
      st.environments = envs
      st.error = undefined
      for (const e of envs) {
        const was = before.get(e.id)
        if (was && was.phase !== e.phase) this.emit('phase', st, e, was)
      }
    } catch (err) {
      if (err instanceof api.SignedOut) {
        st.signedIn = false
        st.me = undefined
        st.environments = []
        st.error = undefined
      } else {
        st.error = (err as Error).message
        // Asked again from the top next time: the server may have changed.
        st.compatibility = undefined
      }
    }
  }
}

export const watch = new Watch()
