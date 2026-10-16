// How often a live socket is checked, how long its answer may take, and how
// long it may take to open. A phone's network can be slow without being
// gone, so an answer has several seconds.
const every = 15_000
const answerWithin = 8_000
const openWithin = 15_000

// Heartbeat tells when a socket to the server has gone without saying so.
// The browser answers the server's pings itself and never tells the page
// whether any came, so a network gone leaves a socket open on this side,
// showing a terminal or a desktop as live and sending what is typed into
// nothing, until TCP gives up minutes later. A heartbeat sends a "ping" of
// its own, as text, which the server answers "pong", and calls lost when an
// answer does not come in time, when the socket does not open in time, or
// at once when the browser goes offline. It checks again as soon as the
// page is shown or the network is back, which is when a socket is most
// likely to have died unseen; while the page is hidden it does not check.
//
// The owner passes each text message to answered, and stops the heartbeat
// when it is done with the socket.
export class Heartbeat {
  private timer: ReturnType<typeof setTimeout> | undefined
  private waiting: ReturnType<typeof setTimeout> | undefined
  private stopped = false
  private ws: WebSocket
  private lost: (why: string) => void

  constructor(ws: WebSocket, lost: (why: string) => void) {
    this.ws = ws
    this.lost = lost
    document.addEventListener('visibilitychange', this.check)
    window.addEventListener('online', this.check)
    window.addEventListener('offline', this.offline)
    if (ws.readyState === WebSocket.CONNECTING) {
      this.waiting = setTimeout(() => this.fail('could not connect'), openWithin)
      ws.addEventListener('open', () => {
        clearTimeout(this.waiting)
        this.waiting = undefined
        this.later()
      })
    } else this.later()
  }

  // answered says whether a text message was the answer to a ping, which
  // the owner then ignores.
  answered(data: unknown): boolean {
    if (data !== 'pong') return false
    clearTimeout(this.waiting)
    this.waiting = undefined
    this.later()
    return true
  }

  stop() {
    this.stopped = true
    clearTimeout(this.timer)
    clearTimeout(this.waiting)
    document.removeEventListener('visibilitychange', this.check)
    window.removeEventListener('online', this.check)
    window.removeEventListener('offline', this.offline)
  }

  private later() {
    clearTimeout(this.timer)
    this.timer = setTimeout(this.check, every)
  }

  private check = () => {
    if (this.stopped || this.waiting !== undefined || document.hidden) return
    if (this.ws.readyState !== WebSocket.OPEN) return
    clearTimeout(this.timer)
    try {
      this.ws.send('ping')
    } catch {
      this.fail('the connection dropped')
      return
    }
    this.waiting = setTimeout(() => this.fail('the connection stopped answering'), answerWithin)
  }

  private offline = () => this.fail('the network is down')

  private fail(why: string) {
    if (this.stopped) return
    this.stop()
    this.lost(why)
  }
}

// onReturn calls fn when the page is shown again or the network comes
// back: the moments to try a dropped connection again at once rather than
// when its backoff runs out. It returns how to stop.
export function onReturn(fn: () => void): () => void {
  const shown = () => {
    if (!document.hidden) fn()
  }
  document.addEventListener('visibilitychange', shown)
  window.addEventListener('online', shown)
  return () => {
    document.removeEventListener('visibilitychange', shown)
    window.removeEventListener('online', shown)
  }
}
