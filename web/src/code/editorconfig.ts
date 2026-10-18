import type { CodeClient } from './client'

// EditorConfig (editorconfig.org): the .editorconfig files from a file's
// folder up to the filesystem's root, or to one that says root = true, each
// a list of sections whose glob names the files its properties are for, a
// nearer file's and a later section's winning. What the editor takes from
// them is how to indent: indent_style, indent_size and tab_width.

export type EditorConfig = {
  indentStyle?: 'tab' | 'space'
  // indentSize is a number of columns, or 'tab' for tab_width's.
  indentSize?: number | 'tab'
  tabWidth?: number
}

type Section = { match: RegExp; props: Record<string, string> }
type Parsed = { root: boolean; sections: Section[] }

// parse reads an .editorconfig file: INI, with ; and # comments.
export function parse(text: string): Parsed {
  let root = false
  const sections: Section[] = []
  let current: Section | null = null
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim()
    if (!line || line.startsWith('#') || line.startsWith(';')) continue
    const head = /^\[(.*)\]$/.exec(line)
    if (head) {
      current = { match: globRegExp(head[1]), props: {} }
      sections.push(current)
      continue
    }
    const kv = /^([^=:]+)[=:](.*)$/.exec(line)
    if (!kv) continue
    const key = kv[1].trim().toLowerCase()
    const value = kv[2].trim().toLowerCase()
    if (current) current.props[key] = value
    else if (key === 'root') root = value === 'true'
  }
  return { root, sections }
}

// globRegExp is an EditorConfig glob as a pattern for a path from the
// .editorconfig's folder: one with no slash matches a file's name in any
// folder below; *, **, ?, [...], [!...], {a,b} and {1..9} as the
// specification has them.
export function globRegExp(glob: string): RegExp {
  let g = glob
  if (!g.includes('/')) g = '**/' + g
  else if (g.startsWith('/')) g = g.slice(1)
  let out = ''
  let braces = 0
  for (let i = 0; i < g.length; i++) {
    const c = g[i]
    if (c === '\\' && i + 1 < g.length) {
      out += escape(g[++i])
    } else if (c === '*') {
      if (g[i + 1] === '*') {
        i++
        // **/ matches no folders too.
        if (g[i + 1] === '/') {
          i++
          out += '(?:.*/)?'
        } else out += '.*'
      } else out += '[^/]*'
    } else if (c === '?') out += '[^/]'
    else if (c === '[') {
      const end = g.indexOf(']', i + 1)
      if (end < 0) out += '\\['
      else {
        let set = g.slice(i + 1, end)
        if (set.startsWith('!')) set = '^' + set.slice(1)
        out += `[${set.replace(/\\/g, '\\\\')}]`
        i = end
      }
    } else if (c === '{') {
      const end = g.indexOf('}', i + 1)
      const range = end > 0 ? /^(-?\d+)\.\.(-?\d+)$/.exec(g.slice(i + 1, end)) : null
      if (range) {
        const [a, b] = [Number(range[1]), Number(range[2])].sort((x, y) => x - y)
        out += b - a <= 1000 ? `(?:${Array.from({ length: b - a + 1 }, (_, k) => a + k).join('|')})` : '-?\\d+'
        i = end
      } else if (end > 0 && g.slice(i + 1, end).includes(',')) {
        braces++
        out += '(?:'
      } else out += '\\{'
    } else if (c === ',' && braces > 0) out += '|'
    else if (c === '}' && braces > 0) {
      braces--
      out += ')'
    } else out += escape(c)
  }
  return new RegExp(`^${out}$`)
}

const escape = (c: string) => c.replace(/[.*+?^${}()|[\]\\/]/g, '\\$&')

// Each client's .editorconfig files, by folder, read once until something
// in the folder changes.
const caches = new WeakMap<CodeClient, Map<string, Promise<Parsed | null>>>()

function cacheFor(client: CodeClient) {
  let c = caches.get(client)
  if (!c) {
    const cache = new Map<string, Promise<Parsed | null>>()
    c = cache
    caches.set(client, cache)
    client.on('fs.changed', ({ dirs }) => {
      for (const d of dirs) cache.delete(d)
    })
  }
  return c
}

// editorConfig is what the .editorconfig files say of a file, a path from
// the client's root or absolute; null if none says anything.
export async function editorConfig(client: CodeClient, path: string): Promise<EditorConfig | null> {
  const abs = path.startsWith('/') ? path : `${client.root}/${path}`
  const cache = cacheFor(client)
  const dirs: string[] = []
  for (let d = abs.slice(0, abs.lastIndexOf('/')); dirs.length < 24; d = d.slice(0, d.lastIndexOf('/'))) {
    dirs.push(d || '/')
    if (!d) break
  }
  const files = await Promise.all(
    dirs.map((d) => {
      let p = cache.get(d)
      if (!p) {
        p = client.read(`${d === '/' ? '' : d}/.editorconfig`).then(
          (t) => (t === null ? null : parse(t)),
          () => null,
        )
        cache.set(d, p)
      }
      return p
    }),
  )
  // From the outermost that counts inward, so nearer files win.
  const props: Record<string, string> = {}
  let top = files.findIndex((f) => f?.root)
  if (top < 0) top = files.length - 1
  for (let i = top; i >= 0; i--) {
    const f = files[i]
    if (!f) continue
    const rel = abs.slice(dirs[i] === '/' ? 1 : dirs[i].length + 1)
    for (const s of f.sections) if (s.match.test(rel)) Object.assign(props, s.props)
  }
  const out: EditorConfig = {}
  if (props.indent_style === 'tab' || props.indent_style === 'space') out.indentStyle = props.indent_style
  const size = Number(props.indent_size)
  if (props.indent_size === 'tab') out.indentSize = 'tab'
  else if (size > 0 && size <= 16) out.indentSize = size
  const width = Number(props.tab_width)
  if (width > 0 && width <= 16) out.tabWidth = width
  return Object.keys(out).length ? out : null
}
