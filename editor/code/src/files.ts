import { watch, type FSWatcher } from 'node:fs'
import { mkdir, readdir, rename, rm, stat, writeFile } from 'node:fs/promises'
import { dirname, isAbsolute, join, relative, resolve } from 'node:path'

export type Entry = { name: string; dir: boolean; size: number }

// Directories whose contents are never listed as changing: they churn, and
// nobody browses them.
const QUIET = new Set(['.git', 'node_modules', '.cache', 'target', 'dist', '__pycache__'])

// Files lists and changes the environment's files. Paths are absolute, or
// relative to the root.
export class Files {
	private listeners = new Set<(dirs: string[]) => void>()
	private watcher: FSWatcher | undefined
	private pending = new Set<string>()
	private timer: NodeJS.Timeout | undefined

	constructor(private root: string) {}

	abs(path: string): string {
		return isAbsolute(path) ? resolve(path) : resolve(this.root, path)
	}

	async list(path: string): Promise<Entry[]> {
		const dir = this.abs(path)
		const ents = await readdir(dir, { withFileTypes: true })
		const out: Entry[] = []
		for (const e of ents) {
			let isDir = e.isDirectory()
			let size = 0
			if (e.isSymbolicLink() || e.isFile()) {
				try {
					const st = await stat(join(dir, e.name))
					isDir = st.isDirectory()
					size = st.size
				} catch {
					// A dangling link is listed as a file.
				}
			}
			out.push({ name: e.name, dir: isDir, size })
		}
		out.sort((a, b) => (a.dir === b.dir ? a.name.localeCompare(b.name) : a.dir ? -1 : 1))
		return out
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

	// onChange calls fn with the directories whose contents changed, a few at
	// a time. The watch starts with the first listener.
	onChange(fn: (dirs: string[]) => void): () => void {
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
		this.pending.clear()
		for (const fn of this.listeners) fn(dirs)
	}

	relative(path: string): string {
		return relative(this.root, this.abs(path))
	}
}
