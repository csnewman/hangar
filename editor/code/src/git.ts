import { execFile } from 'node:child_process'
import { watch, type FSWatcher } from 'node:fs'
import { readFile } from 'node:fs/promises'
import { isAbsolute, join, relative, resolve } from 'node:path'

export type Change = {
	path: string
	// The porcelain v2 status letters: index then working tree. '?' for an
	// untracked file, 'U' for a conflict.
	index: string
	worktree: string
	// For a rename, the path it had.
	from?: string
}

export type Status = {
	repo: boolean
	branch?: string
	ahead?: number
	behind?: number
	changes: Change[]
}

function run(cwd: string, args: string[], input?: string): Promise<string> {
	return new Promise((resolvePromise, reject) => {
		const child = execFile('git', args, { cwd, maxBuffer: 64 << 20, encoding: 'utf8' }, (err, stdout, stderr) => {
			if (err) reject(new Error((stderr || err.message).trim()))
			else resolvePromise(stdout)
		})
		if (input !== undefined) child.stdin?.end(input)
	})
}

// Git answers for the repository at the root.
export class Git {
	private listeners = new Set<() => void>()
	private watchers: FSWatcher[] = []
	private timer: NodeJS.Timeout | undefined

	constructor(private root: string) {}

	private rel(path: string): string {
		return relative(this.root, isAbsolute(path) ? resolve(path) : resolve(this.root, path))
	}

	async status(): Promise<Status> {
		let out: string
		try {
			out = await run(this.root, ['status', '--porcelain=v2', '--branch', '-z', '--untracked-files=all'])
		} catch {
			return { repo: false, changes: [] }
		}
		const status: Status = { repo: true, changes: [] }
		const parts = out.split('\0')
		for (let i = 0; i < parts.length; i++) {
			const line = parts[i]
			if (!line) continue
			if (line.startsWith('# branch.head ')) status.branch = line.slice(14)
			else if (line.startsWith('# branch.ab ')) {
				const [a, b] = line.slice(12).split(' ')
				status.ahead = Number(a.slice(1))
				status.behind = Number(b.slice(1))
			} else if (line.startsWith('1 ')) {
				const f = line.split(' ')
				status.changes.push({ index: f[1][0], worktree: f[1][1], path: f.slice(8).join(' ') })
			} else if (line.startsWith('2 ')) {
				const f = line.split(' ')
				status.changes.push({ index: f[1][0], worktree: f[1][1], path: f.slice(9).join(' '), from: parts[++i] })
			} else if (line.startsWith('u ')) {
				const f = line.split(' ')
				status.changes.push({ index: 'U', worktree: 'U', path: f.slice(10).join(' ') })
			} else if (line.startsWith('? ')) {
				status.changes.push({ index: '?', worktree: '?', path: line.slice(2) })
			}
		}
		return status
	}

	// diff returns a file as it was at HEAD and as the working tree has it,
	// for a diff view to compare. A file new since HEAD was empty there.
	async diff(path: string): Promise<{ path: string; head: string; working: string }> {
		const rel = this.rel(path)
		let head = ''
		try {
			head = await run(this.root, ['show', `HEAD:${rel}`])
		} catch {
			// Not in HEAD.
		}
		let working = ''
		try {
			working = await readFile(join(this.root, rel), 'utf8')
		} catch {
			// Deleted.
		}
		return { path: rel, head, working }
	}

	async stage(path: string) {
		await run(this.root, ['add', '--', this.rel(path)])
		this.changed()
	}

	async unstage(path: string) {
		await run(this.root, ['restore', '--staged', '--', this.rel(path)])
		this.changed()
	}

	// rollback discards a file's changes in the working tree and index, as
	// IntelliJ's Rollback does. An untracked file is deleted.
	async rollback(path: string) {
		const rel = this.rel(path)
		const st = await this.status()
		const c = st.changes.find((x) => x.path === rel)
		if (c?.index === '?') await run(this.root, ['clean', '-f', '--', rel])
		else await run(this.root, ['restore', '--staged', '--worktree', '--source=HEAD', '--', rel])
		this.changed()
	}

	async commit(message: string) {
		if (!message.trim()) throw new Error('a commit needs a message')
		await run(this.root, ['commit', '-F', '-'], message)
		this.changed()
	}

	// onChange calls fn when what git would report may have changed: the index
	// or HEAD moved. Changes to files themselves arrive through Files.
	onChange(fn: () => void): () => void {
		this.listeners.add(fn)
		if (this.watchers.length === 0) this.start()
		return () => {
			this.listeners.delete(fn)
			if (this.listeners.size === 0) {
				for (const w of this.watchers) w.close()
				this.watchers = []
			}
		}
	}

	private start() {
		for (const target of [join(this.root, '.git'), join(this.root, '.git', 'refs', 'heads')]) {
			try {
				const w = watch(target, () => this.changed())
				w.on('error', () => w.close())
				this.watchers.push(w)
			} catch {
				// Not a repository, or no branches yet.
			}
		}
	}

	private changed() {
		clearTimeout(this.timer)
		this.timer = setTimeout(() => {
			for (const fn of this.listeners) fn()
		}, 150)
	}
}
