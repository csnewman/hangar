import { LanguageDescription } from '@codemirror/language'
import { languages } from '@codemirror/language-data'
import { classHighlighter, highlightCode } from '@lezer/highlight'
import DOMPurify from 'dompurify'
import { Marked, type Token } from 'marked'

// md renders GitHub's flavour: tables, task lists, strikethrough, bare
// links, and math between dollars ($x$ in a line, $$ on lines of their
// own around a block), left as its TeX for renderMath.
const md = new Marked({ gfm: true })

const attr = (s: string) => s.replace(/&/g, '&amp;').replace(/"/g, '&quot;').replace(/</g, '&lt;')

md.use({
  extensions: [
    {
      name: 'mathBlock',
      level: 'block',
      start: (src: string) => src.match(/^\$\$/m)?.index,
      tokenizer(src: string) {
        const m =
          /^\$\$[ \t]*\n([\s\S]+?)\n[ \t]*\$\$[ \t]*(?:\n|$)/.exec(src) ?? /^\$\$([^\n]+?)\$\$[ \t]*(?:\n|$)/.exec(src)
        if (m) return { type: 'mathBlock', raw: m[0], text: m[1].trim() }
      },
      renderer: (t) => `<div class="md-math md-math-block" data-tex="${attr(t.text)}"></div>\n`,
    },
    {
      name: 'mathInline',
      level: 'inline',
      start: (src: string) => src.indexOf('$'),
      tokenizer(src: string) {
        // As GitHub: no space just inside the dollars, and no digit just
        // after, so prices ("$5 and $10") stay prices.
        const m = /^\$(?![\s$])((?:\\.|[^\\$\n])*?[^\s\\])\$(?!\d)/.exec(src) ?? /^\$`([^`\n]+)`\$/.exec(src)
        if (m) return { type: 'mathInline', raw: m[0], text: m[1] }
      },
      renderer: (t) => `<span class="md-math" data-tex="${attr(t.text)}"></span>`,
    },
  ],
})

// purify is the preview's own sanitiser, so its hooks touch nothing else
// on the page. Ids and names are given GitHub's user-content- prefix, so a
// document cannot shadow the page's own.
const purify = DOMPurify(window)

// The prefix a heading's id, and so a link to it, carries.
export const idPrefix = 'user-content-'

// renderMarkdown is a document's HTML, safe to show in the page, with an
// empty marker before each top-level block naming the line it starts on
// (data-line, from 1), for keeping the preview level with the editor.
// src rewrites an image's source: a relative one is a file to read.
export function renderMarkdown(text: string, src: (url: string) => string | null): string {
  const source = text.replace(/\r\n?/g, '\n')
  const tokens = md.lexer(source)
  const marked: Token[] = []
  let pos = 0
  let line = 1
  for (const tok of tokens) {
    const at = tok.raw ? source.indexOf(tok.raw, pos) : -1
    if (at >= 0) {
      for (let i = pos; i < at; i++) if (source.charCodeAt(i) === 10) line++
      pos = at
    }
    if (tok.type !== 'space' && tok.type !== 'def') {
      marked.push({
        type: 'html',
        raw: '',
        pre: false,
        block: true,
        text: `<i class="md-line" data-line="${line}"></i>`,
      })
    }
    marked.push(tok)
    if (at >= 0) {
      for (let i = at; i < at + tok.raw.length; i++) if (source.charCodeAt(i) === 10) line++
      pos = at + tok.raw.length
    }
  }
  const html = md.parser(Object.assign(marked, { links: tokens.links }))
  purify.removeHook('afterSanitizeAttributes')
  purify.addHook('afterSanitizeAttributes', (node) => {
    if (node.tagName === 'IMG') {
      const was = node.getAttribute('src')
      if (was && !external(was)) {
        // Read once it is known, rather than requested of the page's own
        // server meanwhile.
        const url = src(was)
        node.removeAttribute('src')
        if (url) node.setAttribute('src', url)
        else node.setAttribute('data-src', was)
      }
    }
    if (node.tagName === 'A' && node.hasAttribute('href')) node.setAttribute('rel', 'noopener noreferrer')
  })
  // A style sheet or a style restyles the page round the preview, and a
  // form submits it.
  return purify.sanitize(html, { SANITIZE_NAMED_PROPS: true, FORBID_TAGS: ['style', 'form'], FORBID_ATTR: ['style'] })
}

// external is whether a link or image is elsewhere than the environment's
// files.
export function external(url: string): boolean {
  return /^[a-z][a-z0-9+.-]*:/i.test(url) || url.startsWith('//')
}

// slugHeadings gives each heading GitHub's id: its text lower-cased, with
// what is not a letter, digit, space, hyphen or underscore dropped and
// spaces made hyphens, a repeat numbered.
export function slugHeadings(root: HTMLElement) {
  const seen = new Map<string, number>()
  for (const h of root.querySelectorAll<HTMLElement>('h1, h2, h3, h4, h5, h6')) {
    const base = (h.textContent ?? '')
      .trim()
      .toLowerCase()
      .replace(/[^\p{L}\p{N}\s_-]/gu, '')
      .replace(/\s/g, '-')
    const n = seen.get(base) ?? 0
    seen.set(base, n + 1)
    if (!h.id) h.id = idPrefix + (n ? `${base}-${n}` : base)
  }
}

// katex is KaTeX once loaded, with its style sheet: only a document with
// math loads it.
let katex: typeof import('katex').default | null = null
let loadingKatex: Promise<void> | null = null
const loadKatex = () =>
  (loadingKatex ??= Promise.all([import('katex'), import('katex/dist/katex.min.css')]).then(([k]) => {
    katex = k.default
  }))

// renderMath typesets the math in a preview: the $ and $$ spans, and
// fenced blocks of math. done is told once it has, the blocks' heights
// having changed.
export function renderMath(root: HTMLElement, live: () => boolean, done: () => void) {
  for (const code of root.querySelectorAll<HTMLElement>('pre > code.language-math')) {
    const div = document.createElement('div')
    div.className = 'md-math md-math-block'
    div.dataset.tex = code.textContent ?? ''
    code.parentElement!.replaceWith(div)
  }
  const els = [...root.querySelectorAll<HTMLElement>('.md-math')]
  if (!els.length) return
  const typeset = () => {
    if (!live() || !katex) return
    for (const el of els) {
      katex.render(el.dataset.tex ?? '', el, {
        displayMode: el.classList.contains('md-math-block'),
        throwOnError: false,
      })
    }
    done()
  }
  if (katex) typeset()
  else loadKatex().then(typeset, () => {})
}

// Mermaid's drawings, by theme and source, so typing elsewhere in the
// document draws none of them again; and the drawing under way, which
// Mermaid does one at a time.
const drawn = new Map<string, { svg?: string; error?: string }>()
let drawing: Promise<unknown> = Promise.resolve()
let mermaidID = 0

// renderMermaid draws the document's mermaid blocks, as GitHub does, in
// the page's light or dark. Mermaid loads only for a document with one, and
// runs at its strict security level: no scripts or links in a drawing.
export function renderMermaid(root: HTMLElement, dark: boolean, live: () => boolean, done: () => void) {
  for (const code of root.querySelectorAll<HTMLElement>('pre > code.language-mermaid')) {
    const source = code.textContent ?? ''
    const key = `${dark}\0${source}`
    const box = document.createElement('div')
    box.className = 'md-mermaid'
    code.parentElement!.replaceWith(box)
    const show = (d: { svg?: string; error?: string }) => {
      if (d.svg) box.innerHTML = d.svg
      else {
        const pre = document.createElement('pre')
        pre.className = 'md-mermaid-error'
        pre.textContent = d.error ?? ''
        box.replaceChildren(pre)
      }
    }
    const was = drawn.get(key)
    if (was) {
      show(was)
      continue
    }
    box.textContent = 'Drawing…'
    drawing = drawing.then(async () => {
      if (!live() || !box.isConnected) return
      const hit = drawn.get(key)
      if (hit) {
        show(hit)
        done()
        return
      }
      const { default: mermaid } = await import('mermaid')
      mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: dark ? 'dark' : 'default' })
      let d: { svg?: string; error?: string }
      try {
        d = { svg: (await mermaid.render(`md-mermaid-${++mermaidID}`, source)).svg }
      } catch (e) {
        d = { error: e instanceof Error ? e.message : String(e) }
      }
      if (drawn.size > 100) drawn.clear()
      drawn.set(key, d)
      if (!live() || !box.isConnected) return
      show(d)
      done()
    })
  }
}

// highlighted holds code blocks already coloured, by language and text, so
// a keystroke elsewhere in the document colours none of them again.
const highlighted = new Map<string, string>()

// highlightBlocks colours each fenced block whose language CodeMirror
// knows, loading the language first where it has to.
export function highlightBlocks(root: HTMLElement, live: () => boolean) {
  for (const code of root.querySelectorAll<HTMLElement>('pre > code[class*="language-"]')) {
    const name = [...code.classList].find((c) => c.startsWith('language-'))?.slice('language-'.length)
    const desc = name ? LanguageDescription.matchLanguageName(languages, name, true) : null
    if (!desc) continue
    const text = code.textContent ?? ''
    const key = `${desc.name}\0${text}`
    const done = highlighted.get(key)
    if (done !== undefined) {
      code.innerHTML = done
      continue
    }
    const colour = () => {
      if (!live() || !desc.support) return
      const out = document.createElement('code')
      highlightCode(
        text,
        desc.support.language.parser.parse(text),
        classHighlighter,
        (t, classes) => {
          if (!classes) {
            out.append(t)
            return
          }
          const span = document.createElement('span')
          span.className = classes
          span.textContent = t
          out.append(span)
        },
        () => out.append('\n'),
      )
      if (highlighted.size > 500) highlighted.clear()
      highlighted.set(key, out.innerHTML)
      code.innerHTML = out.innerHTML
    }
    if (desc.support) colour()
    else desc.load().then(colour, () => {})
  }
}

// resolvePath is where a relative link in a file leads: from the file's
// folder, or from the project's root for one starting with a slash. It is
// null for a link out of the root.
export function resolvePath(from: string, link: string): string | null {
  const parts = link.startsWith('/') ? [] : from.split('/').slice(0, -1)
  for (const seg of link.split('/')) {
    if (seg === '' || seg === '.') continue
    if (seg === '..') {
      if (!parts.length) return null
      parts.pop()
    } else parts.push(seg)
  }
  return parts.join('/')
}
