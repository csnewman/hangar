// The tab bar, and the switcher that opens over the window from it.
import { esc, tone, type AppState, type Env, type ServerInfo } from './types'

const h = window.hangar
const strip = document.getElementById('strip')!
const overlay = document.getElementById('overlay')!
const query = document.getElementById('query') as HTMLInputElement
const results = document.getElementById('results')!

let state: AppState = { tabs: [], servers: [] }
if (h.platform === 'darwin') strip.classList.add('mac')

// ---- Tabs ----

let dragging: number | null = null

function renderTabs() {
  strip.innerHTML = ''
  state.tabs.forEach((t, i) => {
    const el = document.createElement('div')
    el.className = `tab${t.active ? ' on' : ''}${t.loading ? ' loading' : ''}`
    el.title = t.serverName ? `${t.title} — ${t.serverName}` : t.title
    el.draggable = true
    const icon = t.favicon ? `<img src="${esc(t.favicon)}" alt="">` : '<span class="ph"></span>'
    const server = t.serverName && state.servers.length > 1 ? `<span class="server">${esc(t.serverName)}</span>` : ''
    el.innerHTML = `${icon}<span class="title">${esc(t.title)}</span>${server}<button class="x" aria-label="Close tab">✕</button>`
    el.addEventListener('mousedown', (e) => {
      if (e.button === 0 && !(e.target as HTMLElement).closest('.x')) h.activate(t.id)
    })
    el.addEventListener('auxclick', (e) => {
      if (e.button === 1) h.close(t.id)
    })
    el.querySelector('.x')!.addEventListener('click', (e) => {
      e.stopPropagation()
      h.close(t.id)
    })
    el.addEventListener('dragstart', () => (dragging = t.id))
    el.addEventListener('dragover', (e) => e.preventDefault())
    el.addEventListener('drop', () => {
      if (dragging !== null && dragging !== t.id) h.move(dragging, i)
      dragging = null
    })
    strip.appendChild(el)
  })
  const add = document.createElement('button')
  add.className = 'icon-btn'
  add.title = 'New tab (⌘T)'
  add.textContent = '+'
  add.style.fontSize = '18px'
  add.addEventListener('click', () => newTabMenu(add))
  strip.appendChild(add)
  const spacer = document.createElement('div')
  spacer.className = 'spacer'
  strip.appendChild(spacer)
  const go = document.createElement('button')
  go.className = 'icon-btn'
  go.title = 'Go to an environment (⌘L)'
  go.innerHTML = '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/></svg>'
  go.addEventListener('click', () => h.overlay(true).then(openSwitcher))
  strip.appendChild(go)
}

// newTabMenu is a new tab for one of the servers, or the home page.
function newTabMenu(anchor: HTMLElement) {
  if (state.servers.length === 0) return h.newTab(null)
  const menu = document.createElement('div')
  menu.className = 'switcher'
  menu.style.cssText = `position:fixed;top:40px;left:${anchor.getBoundingClientRect().left}px;width:240px;padding:6px`
  const items: [string, string | null][] = [['Home', null], ...state.servers.map((s): [string, string] => [s.name, s.id])]
  for (const [label, id] of items) {
    const r = document.createElement('div')
    r.className = 'result'
    r.textContent = label
    r.addEventListener('click', () => {
      close()
      h.newTab(id)
    })
    menu.appendChild(r)
  }
  const close = () => {
    menu.remove()
    h.overlay(false)
  }
  overlay.classList.remove('open')
  document.body.appendChild(menu)
  h.overlay(true)
  setTimeout(() => document.addEventListener('mousedown', (e) => !menu.contains(e.target as Node) && close(), { once: true }))
}

// ---- The switcher ----

type Hit = { server: ServerInfo; env: Env; score: number }
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
    for (const e of s.environments) {
      const sc = Math.max(score(q, e.name), score(q, `${e.owner}/${e.name}`) - 0.5, score(q, s.name) - 1)
      if (sc > 0) hits.push({ server: s, env: e, score: sc + (e.own ? 0.25 : 0) + (e.phase === 'running' ? 0.1 : 0) })
    }
  }
  hits.sort((a, b) => b.score - a.score || a.env.name.localeCompare(b.env.name))
  sel = Math.min(sel, Math.max(0, hits.length - 1))
  results.innerHTML = hits.length
    ? ''
    : `<div class="result muted">${state.servers.length ? 'No environments match.' : 'Add a server from the home page first.'}</div>`
  hits.slice(0, 50).forEach((hit, i) => {
    const r = document.createElement('div')
    r.className = `result${i === sel ? ' sel' : ''}`
    const who = hit.env.own ? '' : `${esc(hit.env.owner)}/`
    r.innerHTML = `<span class="dot ${tone(hit.env.phase)}"></span><span class="name">${who}${esc(hit.env.name)}</span>
      <span class="muted small">${esc(hit.env.phase)}</span><span class="where">${esc(hit.server.name)}</span>`
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
