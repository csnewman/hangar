// Tooltips for every element with a title, shown sooner than the browser's
// own, which waits a second or more and cannot be told otherwise.
//
// An element's title is moved into data-tip the first time the pointer is
// over it, so the browser's tooltip never shows; React sets the title again
// only when it changes, and the newer title wins. An element left with no
// accessible name of its own is given the title as its label.

// How long the pointer rests on an element before its tooltip shows, and
// how soon after one hides the next shows at once, as moving along a
// toolbar does.
const SHOW_AFTER_MS = 350
const WARM_MS = 500

let tip: HTMLDivElement | null = null
let target: Element | null = null
let timer: ReturnType<typeof setTimeout> | undefined
let hiddenAt = 0

function textOf(el: Element): string {
  const title = el.getAttribute('title')
  if (title !== null) {
    el.removeAttribute('title')
    if (title) {
      el.setAttribute('data-tip', title)
      if (!el.getAttribute('aria-label') && !el.textContent?.trim()) el.setAttribute('aria-label', title)
    } else el.removeAttribute('data-tip')
  }
  return el.getAttribute('data-tip') ?? ''
}

function place(el: Element) {
  if (!tip) return
  const r = el.getBoundingClientRect()
  const t = tip.getBoundingClientRect()
  const gap = 6
  let top = r.bottom + gap
  if (top + t.height > window.innerHeight - 4) top = r.top - gap - t.height
  let left = r.left + r.width / 2 - t.width / 2
  left = Math.max(4, Math.min(left, window.innerWidth - t.width - 4))
  tip.style.top = `${Math.max(4, top)}px`
  tip.style.left = `${left}px`
}

function show(el: Element) {
  const text = textOf(el)
  if (!text || !el.isConnected) return
  if (!tip) {
    tip = document.createElement('div')
    tip.className = 'app-tip'
    tip.setAttribute('role', 'tooltip')
    document.body.appendChild(tip)
  }
  tip.textContent = text
  tip.hidden = false
  place(el)
}

function hide() {
  clearTimeout(timer)
  if (tip && !tip.hidden) {
    tip.hidden = true
    hiddenAt = Date.now()
  }
  target = null
}

function over(e: PointerEvent) {
  if (e.pointerType === 'touch') return
  // An embedded page's title names it for screen readers; it is no
  // tooltip. Once the pointer is in it, this page hears nothing more of it
  // until it leaves, so a tooltip shown now would never be hidden.
  if (e.target instanceof HTMLIFrameElement) {
    hide()
    return
  }
  const el = (e.target as Element | null)?.closest?.('[title], [data-tip]') ?? null
  if (el === target) return
  const warm = (tip && !tip.hidden) || Date.now() - hiddenAt < WARM_MS
  hide()
  if (!el) return
  // Taken now, before the browser's own tooltip can start.
  textOf(el)
  target = el
  timer = setTimeout(() => target === el && show(el), warm ? 0 : SHOW_AFTER_MS)
}

function out(e: PointerEvent) {
  if (target && !(e.relatedTarget instanceof Node && target.contains(e.relatedTarget))) hide()
}

let installed = false

// installTooltips starts showing tooltips for the whole page.
export function installTooltips() {
  if (installed) return
  installed = true
  document.addEventListener('pointerover', over, true)
  document.addEventListener('pointerout', out, true)
  for (const ev of ['pointerdown', 'keydown', 'wheel'] as const) document.addEventListener(ev, hide, true)
  window.addEventListener('scroll', hide, true)
  window.addEventListener('blur', hide)
}
