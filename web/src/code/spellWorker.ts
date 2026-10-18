// The spelling checker, in a worker of its own: loading the dictionary
// takes about a second, and suggesting a word tens of milliseconds, neither
// of which the editor should wait on.

import nspell from 'nspell'
import affURL from '@dict/en/index.aff?url'
import dicURL from '@dict/en/index.dic?url'
import list0 from '@dict/cspell/dict-software-terms/dict/softwareTerms.txt.gz?url'
import list1 from '@dict/cspell/dict-software-terms/dict/software-tools.txt?url'
import list2 from '@dict/cspell/dict-software-terms/dict/networkingTerms.txt?url'
import list3 from '@dict/cspell/dict-software-terms/dict/webServices.txt?url'
import list4 from '@dict/cspell/dict-software-terms/dict/computing-acronyms.txt?url'
import list5 from '@dict/cspell/dict-software-terms/dict/posix-errno.txt?url'
import list6 from '@dict/cspell/dict-software-terms/dict/posix-signals.txt?url'
import list7 from '@dict/cspell/dict-aws/dict/aws.txt?url'
import list8 from '@dict/cspell/dict-companies/dict/companies.txt?url'
import list9 from '@dict/cspell/dict-css/dict/css.txt?url'
import list10 from '@dict/cspell/dict-docker/dict/docker-words.txt.gz?url'
import list11 from '@dict/cspell/dict-dotnet/dict/dotnet.txt?url'
import list12 from '@dict/cspell/dict-filetypes/filetypes.txt.gz?url'
import list13 from '@dict/cspell/dict-fullstack/dict/fullstack.txt?url'
import list14 from '@dict/cspell/dict-golang/dict/go.txt?url'
import list15 from '@dict/cspell/dict-html/dict/html.txt.gz?url'
import list16 from '@dict/cspell/dict-k8s/dict/k8s.txt?url'
import list17 from '@dict/cspell/dict-lua/dict/lua.txt?url'
import list18 from '@dict/cspell/dict-node/dict/node.txt?url'
import list19 from '@dict/cspell/dict-npm/dict/npm.txt?url'
import list20 from '@dict/cspell/dict-php/dict/php.txt?url'
import list21 from '@dict/cspell/dict-python/dict/python.txt?url'
import list22 from '@dict/cspell/dict-python/dict/python-common.txt?url'
import list23 from '@dict/cspell/dict-ruby/dict/ruby.txt?url'
import list24 from '@dict/cspell/dict-rust/dict/rust.txt.gz?url'
import list25 from '@dict/cspell/dict-shell/dict/shell-all-words.txt?url'
import list26 from '@dict/cspell/dict-sql/sql.txt.gz?url'
import list27 from '@dict/cspell/dict-typescript/dict/typescript.txt?url'

// cspell's lists of software's words: its terms, tools, services and
// companies, and each language's and ecosystem's own.
const lists = [
  list0,
  list1,
  list2,
  list3,
  list4,
  list5,
  list6,
  list7,
  list8,
  list9,
  list10,
  list11,
  list12,
  list13,
  list14,
  list15,
  list16,
  list17,
  list18,
  list19,
  list20,
  list21,
  list22,
  list23,
  list24,
  list25,
  list26,
  list27,
]

export type SpellRequest = { id: number; check: string[] } | { id: number; suggest: string }
export type SpellReply = { id: number; bad?: string[]; suggestions?: string[] }

// text is a file's text, gunzipped if it is gzip's: a server may have
// unpacked it already, saying it was encoded.
const text = async (url: string) => {
  const res = await fetch(url)
  if (!res.ok) throw new Error(`${url}: ${res.status}`)
  const bytes = new Uint8Array(await res.arrayBuffer())
  if (bytes[0] !== 0x1f || bytes[1] !== 0x8b) return new TextDecoder().decode(bytes)
  return new Response(new Blob([bytes]).stream().pipeThrough(new DecompressionStream('gzip'))).text()
}

// words are a cspell list's words, lower-cased, less its comments, its
// words forbidden (!) and its patterns and compounds (* and +).
const words = (list: string) =>
  list
    .split('\n')
    .map((w) => w.trim().replace(/^~/, ''))
    .filter((w) => w && !w.startsWith('#') && !/[!*+\s]/.test(w))
    .map((w) => w.toLowerCase())

const ready = (async () => {
  const [aff, dic, ...more] = await Promise.all([
    text(affURL),
    text(dicURL),
    ...lists.map((url) => text(url).catch(() => '')),
  ])
  return { spell: nspell(aff, dic), known: new Set(more.flatMap(words)) }
})()

self.onmessage = async (e: MessageEvent<SpellRequest>) => {
  const req = e.data
  const { spell, known } = await ready
  const reply: SpellReply = { id: req.id }
  if ('check' in req) reply.bad = req.check.filter((w) => !known.has(w.toLowerCase()) && !spell.correct(w))
  else reply.suggestions = spell.suggest(req.suggest).slice(0, 6)
  self.postMessage(reply)
}
