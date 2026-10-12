// What the Project tree does to files, as IntelliJ's Project view does:
// paste, move and copy, delete, upload and download. Each that changes the
// files says how to undo it: a move moves back, a copy goes in the trash,
// and a deletion, which is to the trash, comes back out of it. Where a
// folder already has a name being put there, the user is asked whether to
// replace it (into the trash, so that too is undone), skip it or keep both.

import type { Clash, CodeClient, Trashed } from './client'

// Undo puts back what an operation did. moved is where undoing it moves
// things, for open tabs to follow.
export type Undo = { label: string; run: () => Promise<void>; moved?: Moved }

// Ask asks the user what to do with the names a folder already has: null
// cancels.
export type Ask = (q: { names: string[]; to: string; op: 'copy' | 'move' | 'upload' }) => Promise<Clash | null>

export const dirOf = (p: string) => (p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '')
export const nameOf = (p: string) => p.slice(p.lastIndexOf('/') + 1)
export const join = (dir: string, name: string) => (dir ? `${dir}/${name}` : name)

const plural = (n: number, one: string) => `${n} ${one}${n === 1 ? '' : 's'}`

// what names the things an operation was on: one by its name, more by
// their number.
export const what = (paths: string[]) => (paths.length === 1 ? nameOf(paths[0]) : plural(paths.length, 'item'))

// The clipboard of files, as IntelliJ's: what was cut or copied, which
// paste puts in a folder. It is the page's, shared by its trees.
export type FileClip = { op: 'copy' | 'cut'; paths: string[] } | null
let clip: FileClip = null
const clipListeners = new Set<() => void>()
export const fileClip = () => clip
export function setFileClip(c: FileClip) {
  clip = c
  for (const fn of clipListeners) fn()
}
export function onFileClip(fn: () => void): () => void {
  clipListeners.add(fn)
  return () => clipListeners.delete(fn)
}

// Moved is where things went, for open tabs to follow.
export type Moved = { from: string; to: string }[]

// transferPaths copies or moves paths into a folder, asking about clashes.
// It returns what went where and how to undo it, or null if cancelled.
export async function transferPaths(
  client: CodeClient,
  op: 'copy' | 'move',
  paths: string[],
  to: string,
  ask: Ask,
  name?: string,
): Promise<{ done: Moved; undo: Undo } | null> {
  let r = await client.transfer(op, paths, to, 'ask', name)
  let replaced: Trashed[] = []
  if (r.clashes.length) {
    const choice = await ask({ names: r.clashes, to, op })
    if (!choice) return null
    if (choice === 'replace') {
      replaced = await client.trash(r.clashes.map((n) => join(to, n)))
      r = await client.transfer(op, paths, to, 'ask', name)
      if (r.clashes.length) throw new Error(`${r.clashes.join(', ')} appeared again in ${to || 'the folder'}`)
    } else r = await client.transfer(op, paths, to, choice, name)
  }
  const done = r.done
  const label = `${op === 'copy' ? 'Copied' : 'Moved'} ${what(done.map((d) => d.from))}`
  return {
    done,
    undo: {
      label,
      moved: op === 'move' ? done.map((d) => ({ from: d.to, to: d.from })) : undefined,
      run: async () => {
        if (op === 'copy') {
          if (done.length) await client.trash(done.map((d) => d.to))
        } else {
          for (const d of [...done].reverse()) {
            const back = await client.transfer('move', [d.to], dirOf(d.from), 'ask', nameOf(d.from))
            if (back.clashes.length) throw new Error(`${nameOf(d.from)} is there again; it was not moved back`)
          }
        }
        if (replaced.length) await client.restore(replaced)
      },
    },
  }
}

// rename gives a path a new name in its folder, refusing one taken.
export async function renamePath(client: CodeClient, path: string, name: string): Promise<{ done: Moved; undo: Undo }> {
  const r = await client.transfer('move', [path], dirOf(path), 'ask', name)
  if (r.clashes.length) throw new Error(`${name} already exists`)
  return {
    done: r.done,
    undo: {
      label: `Renamed ${nameOf(path)} to ${name}`,
      moved: r.done.map((d) => ({ from: d.to, to: d.from })),
      run: async () => {
        for (const d of r.done) {
          const back = await client.transfer('move', [d.to], dirOf(d.from), 'ask', nameOf(d.from))
          if (back.clashes.length) throw new Error(`${nameOf(d.from)} is there again; it was not renamed back`)
        }
      },
    },
  }
}

// trashPaths deletes paths into the trash.
export async function trashPaths(client: CodeClient, paths: string[]): Promise<Undo> {
  const trashed = await client.trash(paths)
  return { label: `Deleted ${what(paths)}`, run: () => client.restore(trashed) }
}

// created is the undo of making paths: they go in the trash.
export function created(client: CodeClient, paths: string[], label: string): Undo {
  return { label, run: async () => void (await client.trash(paths)) }
}

// --- Uploads ---------------------------------------------------------------

// An upload is a file from the user's computer and where under the folder
// it goes: its name, or its path in a folder dropped.
export type Upload = { file: File; rel: string }

// The most of a file sent in one message.
const CHUNK = 4 << 20

function base64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader()
    r.onload = () => resolve(String(r.result).slice(String(r.result).indexOf(',') + 1))
    r.onerror = () => reject(r.error)
    r.readAsDataURL(blob)
  })
}

// freeName is a name for name among taken: "name copy", "name copy 2", the
// extension kept, as the service names a copy.
function freeName(taken: Set<string>, name: string): string {
  if (!taken.has(name)) return name
  const dot = name.lastIndexOf('.')
  const ext = dot > 0 ? name.slice(dot) : ''
  const stem = name.slice(0, name.length - ext.length)
  for (let i = 1; ; i++) {
    const n = `${stem} copy${i > 1 ? ` ${i}` : ''}${ext}`
    if (!taken.has(n)) return n
  }
}

// uploadFiles writes files from the user's computer into a folder, a part
// at a time, reporting progress. Names the folder has are asked about, by
// their first part: a dropped folder clashes as a whole.
export async function uploadFiles(
  client: CodeClient,
  files: Upload[],
  to: string,
  ask: Ask,
  progress: (text: string) => void,
): Promise<{ undo: Undo; tops: string[] } | null> {
  const top = (u: Upload) => u.rel.split('/')[0]
  const tops = [...new Set(files.map(top))]
  const have = new Set((await client.list(to)).map((e) => e.name))
  const clashes = tops.filter((t) => have.has(t))
  let replaced: Trashed[] = []
  const rename = new Map<string, string>()
  if (clashes.length) {
    const choice = await ask({ names: clashes, to, op: 'upload' })
    if (!choice) return null
    if (choice === 'skip') files = files.filter((f) => !clashes.includes(top(f)))
    else if (choice === 'replace') replaced = await client.trash(clashes.map((n) => join(to, n)))
    else {
      const taken = new Set([...have, ...tops])
      for (const c of clashes) {
        const n = freeName(taken, c)
        taken.add(n)
        rename.set(c, n)
      }
    }
  }
  const total = files.reduce((n, f) => n + f.file.size, 0)
  let sent = 0
  const made = new Set<string>()
  try {
    for (const u of files) {
      const parts = u.rel.split('/')
      parts[0] = rename.get(parts[0]) ?? parts[0]
      const path = join(to, parts.join('/'))
      const token = await client.uploadStart(path)
      try {
        for (let at = 0; at < u.file.size; at += CHUNK) {
          await client.uploadWrite(token, await base64(u.file.slice(at, at + CHUNK)))
          sent += Math.min(CHUNK, u.file.size - at)
          progress(`Uploading ${u.file.name} · ${total ? Math.floor((sent / total) * 100) : 100}%`)
        }
        await client.uploadEnd(token, false)
      } catch (e) {
        await client.uploadAbort(token).catch(() => {})
        throw e
      }
      made.add(join(to, parts[0]))
    }
  } finally {
    progress('')
  }
  const out = [...made]
  return {
    tops: out,
    undo: {
      label: `Uploaded ${what(out)}`,
      run: async () => {
        if (out.length) await client.trash(out)
        if (replaced.length) await client.restore(replaced)
      },
    },
  }
}

// filesOf reads what was dropped from the user's computer: files, and
// folders with everything in them.
export async function filesOf(data: DataTransfer): Promise<Upload[]> {
  const entries = [...data.items]
    .filter((i) => i.kind === 'file')
    .map((i) => i.webkitGetAsEntry())
    .filter((e): e is FileSystemEntry => !!e)
  if (entries.length === 0) return [...data.files].map((file) => ({ file, rel: file.name }))
  const out: Upload[] = []
  const walk = async (e: FileSystemEntry, rel: string) => {
    if (e.isFile) {
      const file = await new Promise<File>((res, rej) => (e as FileSystemFileEntry).file(res, rej))
      out.push({ file, rel })
      return
    }
    const reader = (e as FileSystemDirectoryEntry).createReader()
    // A reader gives a folder's entries a batch at a time, then none.
    for (;;) {
      const batch = await new Promise<FileSystemEntry[]>((res, rej) => reader.readEntries(res, rej))
      if (batch.length === 0) break
      for (const c of batch) await walk(c, `${rel}/${c.name}`)
    }
  }
  for (const e of entries) await walk(e, e.name)
  return out
}

// --- Downloads -------------------------------------------------------------

// The most read in one message.
const READ = 8 << 20

// download saves paths to the user's computer: a file as itself, folders
// and several files as a zip of them.
export async function download(
  client: CodeClient,
  paths: { path: string; dir: boolean }[],
  rootName: string,
  progress: (text: string) => void,
) {
  const single = paths.length === 1 && !paths[0].dir
  let src = paths[0].path
  let name = nameOf(src) || rootName
  let zipped: string | null = null
  try {
    if (!single) {
      progress('Packing the download…')
      const z = await client.zip(paths.map((p) => p.path))
      zipped = src = z.path
      name = paths.length === 1 ? `${nameOf(paths[0].path) || rootName}.zip` : `${rootName}.zip`
    }
    const parts: BlobPart[] = []
    let size = Infinity
    for (let at = 0; at < size;) {
      const b = await client.bytes(src, at, READ)
      size = b.size
      const bin = atob(b.data)
      const bytes = new Uint8Array(bin.length)
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i)
      parts.push(bytes)
      if (bytes.length === 0) break
      at += bytes.length
      progress(`Downloading ${name} · ${size ? Math.floor((at / size) * 100) : 100}%`)
    }
    const url = URL.createObjectURL(new Blob(parts))
    const a = document.createElement('a')
    a.href = url
    a.download = name
    document.body.appendChild(a)
    a.click()
    a.remove()
    setTimeout(() => URL.revokeObjectURL(url), 60_000)
  } finally {
    if (zipped) client.unzip(zipped).catch(() => {})
    progress('')
  }
}
