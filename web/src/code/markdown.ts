import { LanguageDescription } from '@codemirror/language'
import { languages } from '@codemirror/language-data'
import { classHighlighter, highlightCode } from '@lezer/highlight'
import DOMPurify from 'dompurify'
import { Marked, type Token } from 'marked'

// md renders GitHub's flavour: tables, task lists, strikethrough and bare
// links.
const md = new Marked({ gfm: true })

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
