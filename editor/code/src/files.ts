import { constants, watch, type FSWatcher } from 'node:fs'
import { access, mkdir, open, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises'
import { dirname, isAbsolute, join, relative, resolve } from 'node:path'

import {
	convert,
	isPacked,
	restore,
	setMode,
	textProps,
	transfer,
	trash,
	Uploads,
	zip,
	type Clash,
	type Encoding,
	type Separator,
	type Trashed,
} from './fileops.js'

// An entry in a folder: whether it is one, its size, and of a file whether
// it is a link, runs, and may be written. ignored is git's say, added by
// the connection.
export type Entry = {
	name: string
	dir: boolean
	size: number
	link?: boolean
	exec?: boolean
	readonly?: boolean
	ignored?: boolean
}

// The largest file read for a preview.
const MAX_READ = 4 << 20

// The most of a file read as bytes at once: an image whole, or a hex
// view's chunk of a larger file.
export const MAX_BYTES = 32 << 20

// What a file is, for choosing how to show it: text, or bytes that are
// not, and how large; whether the user may write it, and its version.
export type Probe = { size: number; binary: boolean; writable: boolean; version: string }

// A file's version is its modification time to the nanosecond and its
// size: a write by anyone changes it.
async function version(p: string): Promise<string> {
	const st = await stat(p, { bigint: true })
	return `${st.mtimeNs}:${st.size}`
}

// Directories whose contents are never listed as changing: they churn, and
// nobody browses them.
const QUIET = new Set(['.git', 'node_modules', '.cache', 'target', 'dist', '__pycache__'])

// Files lists and changes the environment's files. Paths are absolute, or
// relative to the root.
export class Files {
	private listeners = new Set<(dirs: string[], files: string[]) => void>()
	private watcher: FSWatcher | undefined
	private pending = new Set<string>()
	private pendingFiles = new Set<string>()
	private timer: NodeJS.Timeout | undefined
	private uploads = new Uploads()

	constructor(private root: string) {}

	abs(path: string): string {
		return isAbsolute(path) ? resolve(path) : resolve(this.root, path)
	}

	async list(path: string): Promise<Entry[]> {
		const dir = this.abs(path)
		const ents = await readdir(dir, { withFileTypes: true })
		const out: Entry[] = []
		for (const e of ents) {
			const entry: Entry = { name: e.name, dir: e.isDirectory(), size: 0 }
			if (e.isSymbolicLink()) entry.link = true
			if (e.isSymbolicLink() || e.isFile()) {
				try {
					const st = await stat(join(dir, e.name))
					entry.dir = st.isDirectory()
					if (!entry.dir) {
						entry.size = st.size
						if (st.mode & 0o111) entry.exec = true
						if (!(st.mode & 0o222)) entry.readonly = true
					}
				} catch {
					// A dangling link is listed as a file.
				}
			}
			out.push(entry)
		}
		out.sort((a, b) => (a.dir === b.dir ? a.name.localeCompare(b.name) : a.dir ? -1 : 1))
		return out
	}

	// read is a file's text, for a preview; null for a file too large to
	// show or not text.
	async read(path: string): Promise<{ text: string | null }> {
		const p = this.abs(path)
		const st = await stat(p)
		if (!st.isFile() || st.size > MAX_READ) return { text: null }
		const buf = await readFile(p)
		if (buf.subarray(0, 8000).includes(0)) return { text: null }
		return { text: buf.toString('utf8') }
	}

	// probe says whether a file is text: binary when its start holds a NUL,
	// as git judges.
	async probe(path: string): Promise<Probe> {
		const p = this.abs(path)
		const st = await stat(p)
		if (!st.isFile()) throw new Error(`${path} is not a file`)
		const f = await open(p, 'r')
		try {
			const buf = Buffer.alloc(Math.min(8192, st.size))
			const { bytesRead } = await f.read(buf, 0, buf.length, 0)
			let writable = true
			try {
				await access(p, constants.W_OK)
			} catch {
				writable = false
			}
			return { size: st.size, binary: buf.subarray(0, bytesRead).includes(0), writable, version: await version(p) }
		} finally {
			await f.close()
		}
	}

	// bytes is part of a file, base64: length bytes from offset, at most
	// MAX_BYTES, fewer at its end.
	async bytes(
		path: string,
		offset: number,
		length: number,
	): Promise<{ size: number; offset: number; data: string; version: string }> {
		const p = this.abs(path)
		const st = await stat(p)
		if (!st.isFile()) throw new Error(`${path} is not a file`)
		const from = Math.max(0, Math.min(Math.floor(offset), st.size))
		const n = Math.max(0, Math.min(Math.floor(length), MAX_BYTES, st.size - from))
		const f = await open(p, 'r')
		try {
			const buf = Buffer.alloc(n)
			const { bytesRead } = await f.read(buf, 0, n, from)
			return {
				size: st.size,
				offset: from,
				data: buf.subarray(0, bytesRead).toString('base64'),
				version: await version(p),
			}
		} finally {
			await f.close()
		}
	}

	// patch writes bytes over a file's own, in place, each run at its
	// offset, if the file is still the version they were made to; changed
	// since, it refuses, so nobody's write is lost.
	async patch(path: string, at: string, runs: { offset: number; data: string }[]): Promise<{ version: string }> {
		const p = this.abs(path)
		if ((await version(p)) !== at) throw new Error(`${path} changed on disk since it was read`)
		const st = await stat(p)
		const bufs = runs.map((r) => ({ offset: Math.floor(r.offset), buf: Buffer.from(r.data, 'base64') }))
		if (bufs.reduce((n, b) => n + b.buf.length, 0) > MAX_BYTES) throw new Error('too many bytes changed at once')
		for (const b of bufs) {
			if (b.offset < 0 || b.offset + b.buf.length > st.size) throw new Error('a change lies outside the file')
		}
		const f = await open(p, 'r+')
		try {
			for (const b of bufs) await f.write(b.buf, 0, b.buf.length, b.offset)
		} finally {
			await f.close()
		}
		return { version: await version(p) }
	}

	async create(path: string) {
		await writeFile(this.abs(path), '', { flag: 'wx' })
	}

	async mkdir(path: string) {
		await mkdir(this.abs(path), { recursive: true })
	}

	async rename(from: string, to: string) {
		await rename(this.abs(from), this.abs(to))
	}

	async remove(path: string) {
		const p = this.abs(path)
		if (p === this.root || p === '/') throw new Error('refusing to delete the root')
		await rm(p, { recursive: true })
	}

	// transfer copies or moves paths into a folder, as fileops' transfer,
	// the paths it made given as the browser named the ones it was given.
	async transfer(op: 'copy' | 'move', from: string[], to: string, clash: Clash, name?: string) {
		const srcs = from.map((f) => this.guarded(f))
		const r = await transfer(op, srcs, this.abs(to), clash, name)
		return { clashes: r.clashes, done: r.done.map((d) => ({ from: this.named(d.from), to: this.named(d.to) })) }
	}

	// trash puts paths in the desktop's trash; restore puts them back.
	async trash(paths: string[]): Promise<Trashed[]> {
		const out = await trash(paths.map((p) => this.guarded(p)))
		return out.map((t) => ({ ...t, path: this.named(t.path) }))
	}

	async restore(items: Trashed[]) {
		await restore(items.map((t) => ({ id: t.id, path: this.abs(t.path) })))
	}

	uploadStart(path: string) {
		return this.uploads.start(this.abs(path))
	}

	uploadWrite(token: string, data: string) {
		return this.uploads.write(token, data)
	}

	async uploadEnd(token: string, replace: boolean) {
		return this.named(await this.uploads.end(token, replace))
	}

	uploadAbort(token: string) {
		return this.uploads.abort(token)
	}

	// zip packs paths for downloading, into a file the browser reads with
	// bytes and then removes with unzip.
	zip(paths: string[]) {
		return zip(paths.map((p) => this.abs(p)))
	}

	async unzip(path: string) {
		if (await isPacked(path)) await rm(path, { force: true })
	}

	setMode(path: string, change: { exec?: boolean; writable?: boolean }) {
		return setMode(this.abs(path), change)
	}

	textProps(path: string) {
		return textProps(this.abs(path))
	}

	convert(path: string, to: { separator?: Separator; encoding?: Encoding; bom?: boolean }) {
		return convert(this.abs(path), to)
	}

	// guarded is a path to change wholesale -- move, copy, trash -- which
	// the root, and /, are not.
	private guarded(path: string): string {
		const p = this.abs(path)
		if (p === this.root || p === '/') throw new Error('refusing to move the root')
		return p
	}

	// named is an absolute path as the browser names it: from the root when
	// within it.
	private named(abs: string): string {
		const rel = relative(this.root, abs)
		return rel && !rel.startsWith('..') && !isAbsolute(rel) ? rel : abs
	}

	// onChange calls fn with the directories whose contents changed, and the
	// paths that changed in them, a few at a time. The watch starts with the
	// first listener.
	onChange(fn: (dirs: string[], files: string[]) => void): () => void {
		this.listeners.add(fn)
		if (!this.watcher) this.start()
		return () => {
			this.listeners.delete(fn)
			if (this.listeners.size === 0) {
				this.watcher?.close()
				this.watcher = undefined
			}
		}
	}

	private start() {
		try {
			this.watcher = watch(this.root, { recursive: true }, (_event, name) => {
				if (!name) return
				const rel = String(name)
				if (rel.split('/').some((part) => QUIET.has(part))) return
				this.pending.add(dirname(join(this.root, rel)))
				this.pendingFiles.add(join(this.root, rel))
				clearTimeout(this.timer)
				this.timer = setTimeout(() => this.flush(), 150)
			})
			this.watcher.on('error', () => {
				this.watcher?.close()
				this.watcher = undefined
			})
		} catch {
			// The root may not exist yet; the browser lists on demand anyway.
		}
	}

	private flush() {
		const dirs = [...this.pending]
		const files = [...this.pendingFiles]
		this.pending.clear()
		this.pendingFiles.clear()
		for (const fn of this.listeners) fn(dirs, files)
	}

	relative(path: string): string {
		return relative(this.root, this.abs(path))
	}
}
