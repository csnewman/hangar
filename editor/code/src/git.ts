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

export type Branch = {
	name: string
	// The branch it tracks, and how far apart the two are.
	upstream?: string
	ahead?: number
	behind?: number
}

export type Branches = {
	current?: string
	local: Branch[]
	remote: string[]
}

// Network commands answer or fail: nothing may wait on a prompt nobody
// sees. A host seen for the first time is trusted, as a first clone is.
// Reading takes no optional locks: a status that locked the index would be
// seen by the watcher below as a change, and answered with another status.
const quiet = {
	...process.env,
	GIT_OPTIONAL_LOCKS: '0',
	GIT_TERMINAL_PROMPT: '0',
	GIT_SSH_COMMAND: 'ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new',
}

function run(cwd: string, args: string[], input?: string, timeout = 0): Promise<string> {
	return new Promise((resolvePromise, reject) => {
		const child = execFile(
			'git',
			args,
			{ cwd, maxBuffer: 64 << 20, encoding: 'utf8', env: quiet, timeout },
			(err, stdout, stderr) => {
				// git's advice ("hint: ...") is for a terminal; what went wrong
				// is the rest.
				const why = (stderr || err?.message || '')
					.split('\n')
					.filter((l) => !l.startsWith('hint:'))
					.join('\n')
					.trim()
				if (err) reject(new Error(why || err.message))
				else resolvePromise(stdout)
			},
		)
		if (input !== undefined) child.stdin?.end(input)
	})
}

const network = 120_000

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

	// show returns a file as HEAD or the index has it, or null where it
	// has none.
	async show(path: string, from: 'HEAD' | 'index'): Promise<{ text: string | null }> {
		const rel = this.rel(path)
		try {
			return { text: await run(this.root, ['show', `${from === 'HEAD' ? 'HEAD' : ''}:${rel}`]) }
		} catch {
			return { text: null }
		}
	}

	// setIndex stages a file's content as given, whatever the working tree
	// holds: how part of a file's changes is staged.
	async setIndex(path: string, content: string) {
		const rel = this.rel(path)
		const staged = (await run(this.root, ['ls-files', '--stage', '--', rel])).split(' ')[0]
		const mode = /^1[0-7]{5}$/.test(staged) ? staged : '100644'
		const sha = (await run(this.root, ['hash-object', '-w', '--stdin'], content)).trim()
		await run(this.root, ['update-index', '--add', '--cacheinfo', `${mode},${sha},${rel}`])
		this.changed()
	}

	// commit records the index. Amending replaces the last commit, keeping
	// its message when none is given.
	async commit(message: string, amend = false) {
		if (!message.trim() && !amend) throw new Error('a commit needs a message')
		const args = ['commit']
		if (amend) args.push('--amend')
		if (message.trim()) args.push('-F', '-')
		else args.push('--no-edit')
		await run(this.root, args, message.trim() ? message : undefined)
		this.changed()
	}

	// lastMessage is the last commit's message, for amending it.
	async lastMessage(): Promise<string> {
		try {
			return (await run(this.root, ['log', '-1', '--format=%B'])).trimEnd()
		} catch {
			return ''
		}
	}

	async branches(): Promise<Branches> {
		const local: Branch[] = []
		const out = await run(this.root, [
			'for-each-ref',
			'--format=%(refname:short)%09%(upstream:short)%09%(upstream:track,nobracket)%09%(HEAD)',
			'refs/heads',
		])
		let current: string | undefined
		for (const line of out.split('\n')) {
			if (!line) continue
			const [name, upstream, track, head] = line.split('\t')
			const b: Branch = { name }
			if (upstream) {
				b.upstream = upstream
				b.ahead = Number(/ahead (\d+)/.exec(track)?.[1] ?? 0)
				b.behind = Number(/behind (\d+)/.exec(track)?.[1] ?? 0)
			}
			if (head === '*') current = name
			local.push(b)
		}
		const remote = (await run(this.root, ['for-each-ref', '--format=%(refname:short)', 'refs/remotes']))
			.split('\n')
			.filter((r) => r && !r.endsWith('/HEAD') && r.includes('/'))
		return { current, local, remote }
	}

	// checkout switches to a branch. A remote branch with no local one of
	// its name gets one, tracking it.
	async checkout(name: string) {
		const { local, remote } = await this.branches()
		if (local.some((b) => b.name === name)) await run(this.root, ['checkout', name])
		else if (remote.includes(name)) {
			const short = name.slice(name.indexOf('/') + 1)
			if (local.some((b) => b.name === short)) await run(this.root, ['checkout', short])
			else await run(this.root, ['checkout', '--track', name])
		} else throw new Error(`no branch ${name}`)
		this.changed()
	}

	async createBranch(name: string) {
		await run(this.root, ['check-ref-format', '--branch', name])
		await run(this.root, ['checkout', '-b', name])
		this.changed()
	}

	// push sends the current branch to what it tracks, or else to origin
	// (or the only remote) under its own name, which it then tracks.
	async push() {
		const { current, local } = await this.branches()
		if (!current) throw new Error('not on a branch')
		if (local.find((b) => b.name === current)?.upstream) await run(this.root, ['push'], undefined, network)
		else {
			const remotes = (await run(this.root, ['remote'])).split('\n').filter(Boolean)
			const remote = remotes.includes('origin') ? 'origin' : remotes[0]
			if (!remote) throw new Error('the repository has no remote')
			await run(this.root, ['push', '--set-upstream', remote, current], undefined, network)
		}
		this.changed()
	}

	// pull takes what the branch tracks, where that fast-forwards; anything
	// needing a merge is left for the terminal.
	async pull() {
		await run(this.root, ['pull', '--ff-only'], undefined, network)
		this.changed()
	}

	async fetch() {
		await run(this.root, ['fetch', '--all', '--prune'], undefined, network)
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
		for (const target of [
			join(this.root, '.git'),
			join(this.root, '.git', 'refs', 'heads'),
			join(this.root, '.git', 'refs', 'remotes'),
		]) {
			try {
				// A lock file comes and goes around every write, which is
				// seen when the file it guards changes.
				const w = watch(target, (_, name) => {
					if (!name?.toString().endsWith('.lock')) this.changed()
				})
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
