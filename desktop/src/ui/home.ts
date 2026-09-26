// The server picker: the servers the app knows, each opening its window,
// and a server added by its address.
import { esc, type AppState } from './types'

const h = window.hangar
const list = document.getElementById('servers')!
const message = document.getElementById('message')!
const form = document.getElementById('add') as HTMLFormElement
const input = document.getElementById('url') as HTMLInputElement

let state: AppState = { server: null, tabs: [], servers: [] }

const count = (n: number) => `${n} environment${n === 1 ? '' : 's'}`

function render() {
  list.innerHTML = ''
  if (state.servers.length === 0) {
    list.innerHTML = '<section><div class="empty muted">No servers yet. Add one below.</div></section>'
    input.focus()
    return
  }
  const sec = document.createElement('section')
  for (const s of state.servers) {
    const row = document.createElement('div')
    row.className = 'server'
    const c = s.compatibility
    const status =
      c && !c.ok
        ? `<span class="${c.update === 'app' ? 'bad' : 'muted'}">${esc(c.why)}</span>`
        : s.signedIn
          ? `signed in as ${esc(s.username ?? '')} · ${count(s.environments.filter((e) => e.own).length)}`
          : 'not signed in'
    row.innerHTML = `<div class="who"><div class="name">${esc(s.name)}</div><div class="muted small">${esc(s.url)} · ${status}</div></div><div class="buttons"></div>`
    const buttons = row.querySelector('.buttons')!
    const open = document.createElement('button')
    open.className = 'btn btn-primary'
    open.textContent = s.open ? 'Show' : 'Open'
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
    buttons.append(open, remove)
    row.querySelector('.name')!.addEventListener('click', () => h.openServer(s.id))
    sec.appendChild(row)
  }
  list.appendChild(sec)
}

form.addEventListener('submit', async (e) => {
  e.preventDefault()
  const url = input.value.trim()
  if (!url) return
  message.textContent = ''
  const r = await h.addServer(url)
  if (r.error) {
    message.className = 'error'
    message.textContent = r.error
  } else {
    input.value = ''
    if (r.warning) {
      message.className = 'warning'
      message.textContent = r.warning
    }
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
