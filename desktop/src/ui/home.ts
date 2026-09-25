// The home page: every server the app knows and the user's environments
// on each, started, stopped and opened from here without going to the
// server's own control panel.
import { esc, tone, type AppState, type Env, type ServerInfo } from './types'

const h = window.hangar
const list = document.getElementById('servers')!
const message = document.getElementById('message')!
const form = document.getElementById('add') as HTMLFormElement
const input = document.getElementById('url') as HTMLInputElement

let state: AppState = { tabs: [], servers: [] }

function formatBytes(n: number): string {
  const units = ['B', 'KB', 'MB', 'GB']
  let i = 0
  while (n >= 1000 && i < units.length - 1) {
    n /= 1000
    i++
  }
  return `${n.toFixed(n < 10 && i > 0 ? 1 : 0)} ${units[i]}`
}

function statusText(e: Env): string {
  if (e.phase !== 'starting') return e.reason ? `${e.phase} · ${e.reason}` : e.phase
  const p = e.progress
  let text = e.reason ?? 'starting'
  if (p?.total) {
    const pct = Math.floor(((p.done ?? 0) / p.total) * 100)
    text += ` · ${pct}%`
    if (p.unit === 'bytes') text += ` of ${formatBytes(p.total)}`
  }
  return text
}

function envRow(s: ServerInfo, e: Env): HTMLElement {
  const row = document.createElement('div')
  row.className = 'env'
  const running = e.phase === 'running'
  const busy = !['running', 'stopped', 'suspended', 'failed'].includes(e.phase)
  const p = e.progress
  const bar = e.phase === 'starting' && p?.total ? `<span class="bar-mini"><span style="width:${Math.min(100, ((p.done ?? 0) / p.total) * 100)}%"></span></span>` : ''
  row.innerHTML = `
    <div><div class="name">${e.own ? '' : esc(e.owner) + '/'}${esc(e.name)}</div><div class="muted small">${esc(e.template)}</div></div>
    <div class="status"><span class="dot ${tone(e.phase)}"></span><span class="text" title="${esc(statusText(e))}">${esc(statusText(e))}</span>${bar}</div>
    <div class="buttons"></div>`
  row.querySelector('.name')!.addEventListener('click', () => h.openEnv(s.id, e.id))
  const buttons = row.querySelector('.buttons')!
  const button = (label: string, fn: () => Promise<unknown>, enabled = true) => {
    const b = document.createElement('button')
    b.className = 'btn'
    b.textContent = label
    b.disabled = !enabled
    b.addEventListener('click', async () => {
      b.disabled = true
      try {
        const err = await fn()
        if (typeof err === 'string') showMessage(err, 'error')
      } catch (x) {
        showMessage((x as Error).message, 'error')
      }
      b.disabled = false
    })
    buttons.appendChild(b)
  }
  button('Open', () => h.openEnv(s.id, e.id))
  button('VS Code', () => h.vscode(s.id, e.id), running)
  if (s.ssh) button('Terminal', () => h.terminal(s.id, e.id), running)
  if (running) button('Stop', () => h.act(s.id, e.id, 'stop'))
  else button(e.phase === 'suspended' ? 'Resume' : 'Start', () => h.act(s.id, e.id, 'start'), !busy)
  return row
}

function render() {
  list.innerHTML = ''
  if (state.servers.length === 0) {
    list.innerHTML = '<section><div class="empty muted">Add a Hangar server above to get started.</div></section>'
    return
  }
  for (const s of state.servers) {
    const sec = document.createElement('section')
    const who = s.signedIn ? `<span class="muted small">signed in as ${esc(s.username ?? '')}</span>` : ''
    sec.innerHTML = `<div class="head"><h2>${esc(s.name)}</h2>${who}<div class="actions"></div></div>`
    const actions = sec.querySelector('.actions')!
    const open = document.createElement('button')
    open.className = 'btn'
    open.textContent = s.signedIn ? 'Control panel' : 'Sign in'
    open.addEventListener('click', () => h.openServer(s.id))
    const remove = document.createElement('button')
    remove.className = 'btn'
    remove.textContent = 'Remove'
    let armed = false
    remove.addEventListener('click', () => {
      if (!armed) {
        armed = true
        remove.textContent = 'Remove from the app?'
        return
      }
      h.removeServer(s.id)
    })
    actions.append(open, remove)

    const c = s.compatibility
    if (c && !c.ok) {
      sec.insertAdjacentHTML('beforeend', `<div class="note ${c.update === 'app' ? 'bad' : 'muted'}">${esc(s.name)} ${esc(c.why)}</div>`)
    } else if (!s.signedIn) {
      sec.insertAdjacentHTML('beforeend', '<div class="note muted">Not signed in. Sign in on its control panel and your environments appear here.</div>')
    } else if (s.error) {
      sec.insertAdjacentHTML('beforeend', `<div class="note bad">${esc(s.error)}</div>`)
    } else {
      const mine = s.environments.filter((e) => e.own)
      const others = s.environments.filter((e) => !e.own)
      if (mine.length === 0) sec.insertAdjacentHTML('beforeend', '<div class="note muted">No environments yet. Create one from the control panel.</div>')
      for (const e of [...mine, ...others]) sec.appendChild(envRow(s, e))
    }
    list.appendChild(sec)
  }
}

function showMessage(text: string, kind: 'error' | 'warning') {
  message.className = kind
  message.textContent = text
}

form.addEventListener('submit', async (e) => {
  e.preventDefault()
  const url = input.value.trim()
  if (!url) return
  message.textContent = ''
  const r = await h.addServer(url)
  if (r.error) showMessage(r.error, 'error')
  else {
    input.value = ''
    if (r.warning) showMessage(r.warning, 'warning')
  }
})

h.onState((s) => {
  state = s
  render()
})
h.get().then((s) => {
  state = s
  render()
})
