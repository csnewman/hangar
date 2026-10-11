import { execFile } from 'node:child_process'
import { watch, type FSWatcher } from 'node:fs'
import { copyFile, mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, join, posix, relative, resolve } from 'node:path'

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

// BranchChanges is what a branch has changed since it left its base: the
// commits since, and every file differing from the base, committed or not.
export type BranchChanges = {
	base: string
	mergeBase: string
	commits: { hash: string; subject: string; author: string; time: number }[]
	changes: { path: string; status: string; from?: string }[]
}

// Blame is who last changed each line of a file: the commits, by hash,
// and each line's commit. Lines not yet committed have the all-zero hash.
export type Blame = {
	commits: Record<string, { author: string; time: number; summary: string }>
	lines: string[]
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

function run(cwd: string, args: string[], input?: string, timeout = 0, env: NodeJS.ProcessEnv = quiet): Promise<string> {
	return new Promise((resolvePromise, reject) => {
		const child = execFile(
			'git',
			args,
			{ cwd, maxBuffer: 64 << 20, encoding: 'utf8', env, timeout },
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

const statusArgs = ['status', '--porcelain=v2', '--branch', '-z', '--untracked-files=all', '--find-renames', '--', '.']

// parseStatus reads porcelain v2 status, its paths made the root's: they
// are from the top of the repository, the root's prefix within it.
function parseStatus(out: string, prefix: string): Status {
	const own = (p: string) => (p.startsWith(prefix) ? p.slice(prefix.length) : posix.relative(prefix, p))
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
			status.changes.push({ index: f[1][0], worktree: f[1][1], path: own(f.slice(8).join(' ')) })
		} else if (line.startsWith('2 ')) {
			const f = line.split(' ')
			status.changes.push({ index: f[1][0], worktree: f[1][1], path: own(f.slice(9).join(' ')), from: own(parts[++i]) })
		} else if (line.startsWith('u ')) {
			const f = line.split(' ')
			status.changes.push({ index: 'U', worktree: 'U', path: own(f.slice(10).join(' ')) })
		} else if (line.startsWith('? ')) {
			status.changes.push({ index: '?', worktree: '?', path: own(line.slice(2)) })
		}
	}
	return status
}

// Git answers for the repository the root is in, which may be the root or
// a folder above it. Paths are the root's: a folder inside a repository sees
// the changes within it, named from it.
export class Git {
	private listeners = new Set<() => void>()
	private watchers: FSWatcher[] = []
	private timer: NodeJS.Timeout | undefined
	private found: Promise<{ prefix: string; gitDir: string }> | undefined

	constructor(private root: string) {}

	// where is the root's place in its repository, its path from the top
	// ("" at the top, else ending in a slash), and the repository's git
	// directory. Not being in a repository is not remembered: one may be
	// made.
	private where(): Promise<{ prefix: string; gitDir: string }> {
		this.found ??= run(this.root, ['rev-parse', '--show-prefix', '--absolute-git-dir']).then(
			(out) => {
				const [prefix, gitDir] = out.split('\n')
				return { prefix: prefix ?? '', gitDir: gitDir ?? join(this.root, '.git') }
			},
			(e) => {
				this.found = undefined
				throw e
			},
		)
		return this.found
	}

	private rel(path: string): string {
		return relative(this.root, isAbsolute(path) ? resolve(path) : resolve(this.root, path))
	}

	async status(): Promise<Status> {
		let where: { prefix: string; gitDir: string }
		let out: string
		try {
			where = await this.where()
			out = await run(this.root, statusArgs)
		} catch {
			return { repo: false, changes: [] }
		}
		let status = parseStatus(out, where.prefix)
		// A file moved without git being told is a deletion and an untracked
		// file, and git finds the rename only among files it knows of. They
		// are made known in a copy of the index, which status is asked again
		// with; the repository's own index is left as it was.
		const untracked = status.changes.filter((c) => c.index === '?')
		if (untracked.length > 0 && untracked.length <= 2000 && status.changes.some((c) => c.worktree === 'D')) {
			try {
				status = await this.withRenames(where, untracked)
			} catch {
				// The plain status stands.
			}
		}
		return status
	}

	private async withRenames(where: { prefix: string; gitDir: string }, untracked: Change[]): Promise<Status> {
		const dir = await mkdtemp(join(tmpdir(), 'hangar-code-index-'))
		try {
			const index = join(dir, 'index')
			try {
				await copyFile(join(where.gitDir, 'index'), index)
			} catch {
				// No index yet: the copy starts empty.
			}
			const env = { ...quiet, GIT_INDEX_FILE: index }
			await run(
				this.root,
				['add', '--intent-to-add', '--pathspec-from-file=-', '--pathspec-file-nul'],
				untracked.map((c) => c.path).join('\0'),
				0,
				env,
			)
			const known = parseStatus(await run(this.root, statusArgs, undefined, 0, env), where.prefix)
			// Known only to the copy, a new file is still untracked.
			const isNew = new Set(untracked.map((c) => c.path))
			for (const c of known.changes) {
				if (c.index === '.' && c.worktree === 'A' && isNew.has(c.path)) {
					c.index = '?'
					c.worktree = '?'
				}
			}
			return known
		} finally {
			await rm(dir, { recursive: true, force: true })
		}
	}

	// change is what status says of a path, renames included.
	private async change(rel: string): Promise<Change | undefined> {
		return (await this.status()).changes.find((c) => c.path === rel)
	}

	// diff returns a file as it was at HEAD, or at another commit, and as
	// the working tree has it, for a diff view to compare. A file new since
	// was empty there; one renamed since is compared with its old name.
	async diff(path: string, against = 'HEAD', from?: string): Promise<{ path: string; head: string; working: string }> {
		const rel = this.rel(path)
		if (!/^[\w./~^-]+$/.test(against) || against.startsWith('-')) throw new Error(`not a revision: ${against}`)
		let head = ''
		try {
			head = await run(this.root, ['show', `${against}:./${from ? this.rel(from) : rel}`])
		} catch {
			if (against !== 'HEAD') {
				return { path: rel, head, working: await readFile(join(this.root, rel), 'utf8').catch(() => '') }
			}
			// Not in HEAD, unless under the name it was renamed from.
			const renamed = (await this.change(rel))?.from
			if (renamed) {
				try {
					head = await run(this.root, ['show', `HEAD:./${renamed}`])
				} catch {
					// New after all.
				}
			}
		}
		let working = ''
		try {
			working = await readFile(join(this.root, rel), 'utf8')
		} catch {
			// Deleted.
		}
		return { path: rel, head, working }
	}

	// stage adds a file's changes to the index; a rename's, both its names.
	async stage(path: string) {
		const rel = this.rel(path)
		const from = (await this.change(rel))?.from
		await run(this.root, ['add', '--all', '--', rel, ...(from ? [from] : [])])
		this.changed()
	}

	async unstage(path: string) {
		const rel = this.rel(path)
		const from = (await this.change(rel))?.from
		await run(this.root, ['restore', '--staged', '--', rel, ...(from ? [from] : [])])
		this.changed()
	}

	// rollback discards a file's changes in the working tree and index, as
	// IntelliJ's Rollback does. An untracked file is deleted.
	async rollback(path: string) {
		const rel = this.rel(path)
		const c = await this.change(rel)
		if (c?.from) {
			// A rename goes back: the old name restored, the new one gone.
			await run(this.root, ['restore', '--staged', '--worktree', '--source=HEAD', '--', c.from])
			await run(this.root, ['rm', '--cached', '--quiet', '--ignore-unmatch', '--', rel])
			await rm(join(this.root, rel), { force: true })
		} else if (c?.index === '?') await run(this.root, ['clean', '-f', '--', rel])
		else await run(this.root, ['restore', '--staged', '--worktree', '--source=HEAD', '--', rel])
		this.changed()
	}

	// show returns a file as HEAD or the index has it, or null where it
	// has none.
	async show(path: string, from: 'HEAD' | 'index'): Promise<{ text: string | null }> {
		const rel = this.rel(path)
		try {
			return { text: await run(this.root, ['show', `${from === 'HEAD' ? 'HEAD' : ''}:./${rel}`]) }
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

	// blame says who last changed each line of a file as it is on disk.
	async blame(path: string): Promise<Blame> {
		const rel = this.rel(path)
		const out = await run(this.root, ['blame', '--porcelain', '--', `./${rel}`])
		const blame: Blame = { commits: {}, lines: [] }
		let hash = ''
		for (const line of out.split('\n')) {
			if (line.startsWith('\t')) {
				blame.lines.push(hash)
				continue
			}
			const head = /^([0-9a-f]{40}) \d+ \d+/.exec(line)
			if (head) {
				hash = head[1]
				blame.commits[hash] ??= { author: '', time: 0, summary: '' }
				continue
			}
			const c = blame.commits[hash]
			if (!c) continue
			if (line.startsWith('author ')) c.author = line.slice(7)
			else if (line.startsWith('author-time ')) c.time = Number(line.slice(12))
			else if (line.startsWith('summary ')) c.summary = line.slice(8)
		}
		return blame
	}

	// defaultBase is the branch others are based on: the remote's default
	// (origin/HEAD), else the first of origin/main, origin/master, main and
	// master there is.
	async defaultBase(): Promise<string | null> {
		try {
			return (await run(this.root, ['symbolic-ref', '--short', 'refs/remotes/origin/HEAD'])).trim()
		} catch {
			// No remote default.
		}
		for (const b of ['origin/main', 'origin/master', 'main', 'master']) {
			try {
				await run(this.root, ['rev-parse', '--verify', '--quiet', `${b}^{commit}`])
				return b
			} catch {
				// Not here.
			}
		}
		return null
	}

	// branchChanges compares the working tree with where it left base (the
	// default base when none is given).
	async branchChanges(base?: string): Promise<BranchChanges> {
		const b = base || (await this.defaultBase())
		if (!b) throw new Error('there is no main branch to compare with')
		if (b.startsWith('-')) throw new Error(`not a branch: ${b}`)
		const mergeBase = (await run(this.root, ['merge-base', 'HEAD', b])).trim()
		const log = await run(this.root, ['log', '--max-count=200', '--format=%h%x1f%s%x1f%an%x1f%ct', `${mergeBase}..HEAD`])
		const commits = log
			.split('\n')
			.filter(Boolean)
			.map((l) => {
				const [hash, subject, author, time] = l.split('\x1f')
				return { hash, subject, author, time: Number(time) }
			})
		const out = await run(this.root, ['diff', '-z', '--name-status', '--find-renames', '--relative', mergeBase, '--', '.'])
		const parts = out.split('\0')
		const changes: BranchChanges['changes'] = []
		for (let i = 0; i < parts.length; i++) {
			const status = parts[i]
			if (!status) continue
			if (status.startsWith('R') || status.startsWith('C')) {
				const from = parts[++i]
				const path = parts[++i]
				changes.push({ path, status: status[0], from })
			} else changes.push({ path: parts[++i], status: status[0] })
		}
		// Files git does not know of yet are the branch's too, and one moved
		// without git is a rename of the file the diff has deleted.
		for (const c of (await this.status()).changes) {
			if (changes.some((x) => x.path === c.path)) continue
			if (c.index === '?') changes.push({ path: c.path, status: 'A' })
			else if (c.index === '.' && c.worktree === 'R' && c.from) {
				const gone = changes.findIndex((x) => x.path === c.from && x.status === 'D')
				if (gone >= 0) changes.splice(gone, 1, { path: c.path, status: 'R', from: c.from })
				else changes.push({ path: c.path, status: 'A' })
			}
		}
		changes.sort((x, y) => x.path.localeCompare(y.path))
		return { base: b, mergeBase, commits, changes }
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
		this.where().then(
			({ gitDir }) => {
				if (this.listeners.size > 0 && this.watchers.length === 0) this.watchGit(gitDir)
			},
			() => {
				// Not a repository.
			},
		)
	}

	private watchGit(gitDir: string) {
		for (const target of [gitDir, join(gitDir, 'refs', 'heads'), join(gitDir, 'refs', 'remotes')]) {
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
