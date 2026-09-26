// A server window's tab bar -- its control panel, then one tab per
// environment -- and the switcher that opens over the window from it.
import { esc, tone, type AppState, type Env, type ServerInfo } from './types'

const h = window.hangar
const strip = document.getElementById('strip')!
const overlay = document.getElementById('overlay')!
const query = document.getElementById('query') as HTMLInputElement
const results = document.getElementById('results')!

let state: AppState = { server: null, tabs: [], servers: [] }
if (h.platform === 'darwin') strip.classList.add('mac')

const mine = () => state.servers.find((s) => s.id === state.server)

// ---- Tabs ----

let dragging: number | null = null

function renderTabs() {
  const server = mine()
  const envs = new Map((server?.environments ?? []).map((e) => [e.id, e]))
  strip.innerHTML = ''
  state.tabs.forEach((t, i) => {
    const el = document.createElement('div')
    const env = t.env ? envs.get(t.env) : undefined
    el.className = `tab${t.panel ? ' panel' : ''}${t.active ? ' on' : ''}${t.loading ? ' loading' : ''}`
    let icon: string
    let label: string
    if (t.panel) {
      icon = t.favicon ? `<img src="${esc(t.favicon)}" alt="">` : '<span class="ph"></span>'
      label = esc(server?.name ?? t.title)
      el.title = `${server?.name ?? ''} — ${t.title}`
    } else {
      icon = `<span class="dot ${tone(env?.phase ?? '')}"></span>`
      label = esc(env ? env.name : t.title)
      el.title = env ? `${env.name} — ${env.phase}${env.reason ? ': ' + env.reason : ''}` : t.title
    }
    const close = t.panel ? '' : '<button class="x" aria-label="Close tab">✕</button>'
    el.innerHTML = `${icon}<span class="title">${label}</span>${close}`
    el.addEventListener('mousedown', (e) => {
      if (e.button === 0 && !(e.target as HTMLElement).closest('.x')) h.activate(t.id)
    })
    if (!t.panel) {
      el.addEventListener('auxclick', (e) => {
        if (e.button === 1) h.close(t.id)
      })
      el.querySelector('.x')!.addEventListener('click', (e) => {
        e.stopPropagation()
        h.close(t.id)
      })
      el.draggable = true
      el.addEventListener('dragstart', () => (dragging = t.id))
    }
    el.addEventListener('dragover', (e) => e.preventDefault())
    el.addEventListener('drop', () => {
      if (dragging !== null && dragging !== t.id) h.move(dragging, Math.max(1, i))
      dragging = null
    })
    strip.appendChild(el)
  })
  const add = document.createElement('button')
  add.className = 'icon-btn'
  add.title = 'Open an environment in a new tab (⌘T)'
  add.textContent = '+'
  add.style.fontSize = '18px'
  add.addEventListener('click', () => h.overlay(true).then(openSwitcher))
  strip.appendChild(add)
}

// ---- The switcher ----

type Hit = { server: ServerInfo; env: Env; here: boolean; score: number }
let hits: Hit[] = []
let sel = 0

function score(q: string, text: string): number {
  if (!q) return 1
  const t = text.toLowerCase()
  const i = t.indexOf(q)
  if (i === 0) return 3
  if (i > 0) return 2
  // Every letter of the query, in order.
  let at = 0
  for (const c of q) {
    at = t.indexOf(c, at)
    if (at < 0) return 0
    at++
  }
  return 1
}

function renderResults() {
  const q = query.value.trim().toLowerCase()
  hits = []
  for (const s of state.servers) {
    const here = s.id === state.server
    for (const e of s.environments) {
      const sc = Math.max(score(q, e.name), score(q, `${e.owner}/${e.name}`) - 0.5)
      // This window's server comes first; another's opens in its window.
      if (sc > 0) hits.push({ server: s, env: e, here, score: sc + (here ? 10 : 0) + (e.own ? 0.25 : 0) + (e.phase === 'running' ? 0.1 : 0) })
    }
  }
  hits.sort((a, b) => b.score - a.score || a.env.name.localeCompare(b.env.name))
  sel = Math.min(sel, Math.max(0, hits.length - 1))
  results.innerHTML = hits.length ? '' : '<div class="result muted">No environments match.</div>'
  let shownOther = false
  hits.slice(0, 50).forEach((hit, i) => {
    if (!hit.here && !shownOther) {
      shownOther = true
      results.insertAdjacentHTML('beforeend', '<div class="heading">Other servers — open in their windows</div>')
    }
    const r = document.createElement('div')
    r.className = `result${i === sel ? ' sel' : ''}`
    const who = hit.env.own ? '' : `${esc(hit.env.owner)}/`
    const where = hit.here ? '' : `<span class="where">${esc(hit.server.name)}</span>`
    r.innerHTML = `<span class="dot ${tone(hit.env.phase)}"></span><span class="name">${who}${esc(hit.env.name)}</span>
      <span class="muted small">${esc(hit.env.phase)}</span>${where}`
    r.addEventListener('mousemove', () => {
      if (sel !== i) {
        sel = i
        renderResults()
      }
    })
    r.addEventListener('click', () => choose(hit, 'open'))
    results.appendChild(r)
  })
}

function openSwitcher() {
  overlay.classList.add('open')
  query.value = ''
  sel = 0
  renderResults()
  query.focus()
}

function closeSwitcher() {
  overlay.classList.remove('open')
  h.overlay(false)
}

async function choose(hit: Hit, how: 'open' | 'vscode' | 'terminal') {
  closeSwitcher()
  if (how === 'open') await h.openEnv(hit.server.id, hit.env.id)
  else if (how === 'vscode') await h.vscode(hit.server.id, hit.env.id)
  else await h.terminal(hit.server.id, hit.env.id)
}

query.addEventListener('input', () => {
  sel = 0
  renderResults()
})
query.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') closeSwitcher()
  else if (e.key === 'ArrowDown') {
    sel = Math.min(sel + 1, hits.length - 1)
    renderResults()
    e.preventDefault()
  } else if (e.key === 'ArrowUp') {
    sel = Math.max(sel - 1, 0)
    renderResults()
    e.preventDefault()
  } else if (e.key === 'Enter' && hits[sel]) {
    choose(hits[sel], e.metaKey || e.ctrlKey ? 'vscode' : e.altKey ? 'terminal' : 'open')
  }
})
overlay.addEventListener('mousedown', (e) => {
  if (e.target === overlay) closeSwitcher()
})

h.onOverlay(openSwitcher)
h.onState((s) => {
  state = s
  renderTabs()
  if (overlay.classList.contains('open')) renderResults()
})
h.get().then((s) => {
  state = s
  renderTabs()
})
