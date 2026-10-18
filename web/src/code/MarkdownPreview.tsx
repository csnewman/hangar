import type { EditorView } from '@codemirror/view'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'

import type { CodeClient } from './client'
import { toURL } from './ImageView'
import { isDark, onDarkChange } from '../theme'
import {
  external,
  highlightBlocks,
  idPrefix,
  renderMarkdown,
  renderMath,
  renderMermaid,
  resolvePath,
  slugHeadings,
} from './markdown'
import { imageType } from './views'

// The largest image read into a preview: the service's limit for bytes at
// once.
const maxImage = 32 << 20

// isMarkdown is whether a file is Markdown, by its name.
export const isMarkdown = (path: string) => /\.(md|markdown|mdown|mkd|mdx)$/i.test(path)

// MarkdownPreview shows Markdown as GitHub renders it, its raw HTML made
// safe first. An image beside the file is read from the environment; a
// link to a file opens it, one to a heading scrolls there, and any other
// opens in a new window. Given the editor beside it, each keeps the other
// at the same place in the document as either is scrolled.
export function MarkdownPreview({
  client,
  path,
  text,
  editor,
  onOpen,
}: {
  client: CodeClient
  path: string
  text: string
  editor?: EditorView | null
  onOpen: (path: string) => void
}) {
  const box = useRef<HTMLDivElement>(null)
  const images = useRef(new Map<string, string | null>())
  const [loaded, setLoaded] = useState(0)
  // Diagrams are drawn in the page's light or dark.
  const [dark, setDark] = useState(isDark)
  useEffect(() => onDarkChange(setDark), [])
  // lock is the side being scrolled to match the other, whose own scroll
  // events are not followed back until it settles.
  const lock = useRef<{ side: 'editor' | 'preview'; until: number } | null>(null)

  useEffect(() => {
    const cache = images.current
    return () => {
      for (const url of cache.values()) if (url) URL.revokeObjectURL(url)
      cache.clear()
    }
  }, [])

  // src is an image's address in the preview: the file read, once it has
  // been.
  const src = (link: string): string | null => {
    const file = resolvePath(path, decodeURI(link.split(/[?#]/)[0]))
    const type = file === null ? undefined : imageType(file)
    if (file === null || !type) return null
    const cache = images.current
    if (cache.has(file)) return cache.get(file) ?? null
    cache.set(file, null)
    client.bytes(file, 0, maxImage).then(
      (b) => {
        if (b.size > maxImage || !images.current.has(file)) return
        images.current.set(file, toURL(b.data, type))
        setLoaded((n) => n + 1)
      },
      () => {},
    )
    return null
  }

  // markers are where each block begins, by its line in the source and
  // its distance from the top of the preview.
  const markers = () => {
    const el = box.current
    if (!el) return []
    const top = el.getBoundingClientRect().top - el.scrollTop
    return [...el.querySelectorAll<HTMLElement>('.md-line')].map((m) => ({
      line: Number(m.dataset.line),
      y: m.getBoundingClientRect().top - top,
    }))
  }

  // followEditor scrolls the preview to the block at the top of the
  // editor, as far through it as the editor is.
  const followEditor = () => {
    const el = box.current
    if (!el || !editor) return
    const sc = editor.scrollDOM
    if (sc.scrollTop + sc.clientHeight >= sc.scrollHeight - 2) {
      lock.current = { side: 'preview', until: performance.now() + 150 }
      el.scrollTop = el.scrollHeight
      return
    }
    const block = editor.lineBlockAtHeight(sc.scrollTop - editor.documentPadding.top)
    const at =
      editor.state.doc.lineAt(block.from).number +
      Math.min(Math.max((sc.scrollTop - block.top) / (block.height || 1), 0), 1)
    const ms = markers()
    const end = { line: editor.state.doc.lines + 1, y: el.scrollHeight }
    let a = { line: 1, y: 0 }
    let b = ms[0] ?? end
    for (let i = 0; i < ms.length && ms[i].line <= at; i++) {
      a = ms[i]
      b = ms[i + 1] ?? end
    }
    const y = a.y + ((b.y - a.y) * (at - a.line)) / Math.max(b.line - a.line, 1)
    lock.current = { side: 'preview', until: performance.now() + 150 }
    el.scrollTop = y
  }

  // followPreview is followEditor the other way.
  const followPreview = () => {
    const el = box.current
    if (!el || !editor) return
    const sc = editor.scrollDOM
    lock.current = { side: 'editor', until: performance.now() + 150 }
    if (el.scrollTop + el.clientHeight >= el.scrollHeight - 2) {
      sc.scrollTop = sc.scrollHeight
      return
    }
    const ms = markers()
    const end = { line: editor.state.doc.lines + 1, y: el.scrollHeight }
    let a = { line: 1, y: 0 }
    let b = ms[0] ?? end
    for (let i = 0; i < ms.length && ms[i].y <= el.scrollTop; i++) {
      a = ms[i]
      b = ms[i + 1] ?? end
    }
    const at = a.line + ((b.line - a.line) * (el.scrollTop - a.y)) / Math.max(b.y - a.y, 1)
    const doc = editor.state.doc
    const n = Math.min(Math.max(Math.floor(at), 1), doc.lines)
    const block = editor.lineBlockAt(doc.line(n).from)
    sc.scrollTop = block.top + editor.documentPadding.top + block.height * (at - Math.floor(at))
  }
  const follow = useRef({ followEditor, followPreview })
  follow.current = { followEditor, followPreview }

  useLayoutEffect(() => {
    const el = box.current
    if (!el) return
    el.innerHTML = renderMarkdown(text, src)
    slugHeadings(el)
    let live = true
    highlightBlocks(el, () => live)
    // Typeset math and drawn diagrams change heights: the preview is put
    // level with the editor again once they are in.
    const again = () => follow.current.followEditor()
    renderMath(el, () => live, again)
    renderMermaid(el, dark, () => live, again)
    follow.current.followEditor()
    return () => {
      live = false
    }
    // src reads the images loaded, which loaded counts.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [text, loaded, dark])

  // Each side follows the other while it is the one being scrolled.
  useEffect(() => {
    const el = box.current
    if (!el || !editor) return
    const held = (side: 'editor' | 'preview') => {
      const l = lock.current
      if (l?.side === side && performance.now() < l.until) return true
      lock.current = null
      return false
    }
    const onEditor = () => held('editor') || follow.current.followEditor()
    const onPreview = () => held('preview') || follow.current.followPreview()
    editor.scrollDOM.addEventListener('scroll', onEditor, { passive: true })
    el.addEventListener('scroll', onPreview, { passive: true })
    // An image loaded, or the width changed, moves what follows.
    const ro = new ResizeObserver(() => follow.current.followEditor())
    ro.observe(el)
    el.addEventListener('load', onEditor, true)
    return () => {
      editor.scrollDOM.removeEventListener('scroll', onEditor)
      el.removeEventListener('scroll', onPreview)
      el.removeEventListener('load', onEditor, true)
      ro.disconnect()
    }
  }, [editor])

  const onClick = (e: React.MouseEvent) => {
    const a = (e.target as HTMLElement).closest('a')
    const href = a?.getAttribute('href')
    if (!a || !href) return
    e.preventDefault()
    if (href.startsWith('#')) {
      const id = decodeURIComponent(href.slice(1))
      const to =
        box.current?.querySelector(`[id="${CSS.escape(idPrefix + id)}"]`) ??
        box.current?.querySelector(`[id="${CSS.escape(id)}"]`)
      to?.scrollIntoView({ block: 'start' })
      return
    }
    if (external(href)) {
      if (/^(https?|mailto):/i.test(href)) window.open(href, '_blank', 'noopener,noreferrer')
      return
    }
    const file = resolvePath(path, decodeURI(href.split(/[?#]/)[0]))
    if (file !== null) onOpen(file || '.')
  }

  return <div ref={box} className="md-preview" onClick={onClick} />
}
