// The browser end of a terminal session: a WebSocket to the server, which
// carries the internal/terminal protocol with each frame as one message.

export interface SessionInfo {
  id: string
  title: string
  created: string
  clients: number
  cols: number
  rows: number
}

interface Reply {
  err?: string
  session?: SessionInfo
}

// Frame types, as internal/terminal defines them.
const INPUT = 0x69 // 'i'
const RESIZE = 0x72 // 'r'
const OUTPUT = 0x6f // 'o'
const EXIT = 0x78 // 'x'

export interface ConnectionEvents {
  // attached is called with the session once the server has attached to it,
  // before any of its output: the output that follows starts with a replay
  // of the session's recent history.
  attached(session: SessionInfo): void
  output(data: Uint8Array): void
  // exited is called when the session's shell exits.
  exited(code: number): void
  // lost is called when the connection drops without the shell exiting,
  // with why if the server said.
  lost(reason: string): void
}

// Connection is one attach to a session. It does not reconnect: the caller
// decides, since a refused attach and a dropped network want different
// answers.
export class Connection {
  private ws: WebSocket
  private done = false
  private encoder = new TextEncoder()

  constructor(
    env: string,
    session: string | undefined,
    dir: string | undefined,
    cols: number,
    rows: number,
    events: ConnectionEvents,
  ) {
    const q = new URLSearchParams({ cols: String(cols), rows: String(rows) })
    if (session) q.set('session', session)
    else if (dir) q.set('dir', dir)
    const scheme = location.protocol === 'https:' ? 'wss' : 'ws'
    this.ws = new WebSocket(`${scheme}://${location.host}/api/frontend/environments/${env}/terminal?${q}`)
    this.ws.binaryType = 'arraybuffer'

    let attached = false
    this.ws.onmessage = (e) => {
      if (typeof e.data === 'string') {
        const reply = JSON.parse(e.data) as Reply
        if (reply.err || !reply.session) {
          this.finish(() => events.lost(reply.err ?? 'the session could not be opened'))
          return
        }
        attached = true
        events.attached(reply.session)
        return
      }
      const msg = new Uint8Array(e.data as ArrayBuffer)
      if (msg.length === 0) return
      if (msg[0] === OUTPUT) {
        events.output(msg.subarray(1))
      } else if (msg[0] === EXIT) {
        const code = msg.length >= 5 ? new DataView(msg.buffer, msg.byteOffset + 1, 4).getInt32(0) : 0
        this.finish(() => events.exited(code))
      }
    }
    this.ws.onclose = (e) => {
      this.finish(() => events.lost(attached ? e.reason || 'the connection dropped' : e.reason || 'could not connect'))
    }
  }

  private finish(report: () => void) {
    if (this.done) return
    this.done = true
    report()
    this.ws.close()
  }

  input(data: string | Uint8Array) {
    const bytes = typeof data === 'string' ? this.encoder.encode(data) : data
    const msg = new Uint8Array(bytes.length + 1)
    msg[0] = INPUT
    msg.set(bytes, 1)
    this.send(msg)
  }

  resize(cols: number, rows: number) {
    const msg = new Uint8Array(5)
    msg[0] = RESIZE
    const v = new DataView(msg.buffer)
    v.setUint16(1, cols)
    v.setUint16(3, rows)
    this.send(msg)
  }

  private send(msg: Uint8Array<ArrayBuffer>) {
    if (this.ws.readyState === WebSocket.OPEN) this.ws.send(msg)
  }

  close() {
    this.done = true
    this.ws.close()
  }
}
