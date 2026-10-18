import type { Dirent } from 'node:fs'
import { readdir, stat } from 'node:fs/promises'
import { basename, isAbsolute, join, posix, relative, resolve } from 'node:path'

import { execFile } from './child.js'
import { Git, type Change } from './git.js'

// RepoKind is how a repository came to be in the folder: the one the folder
// is in, a submodule another names, or a clone of its own.
export type RepoKind = 'folder' | 'submodule' | 'clone'

// RepoChange is a change as the folder sees it, named from the folder. A
// submodule's is its commit having moved, which its parent records.
export type RepoChange = Omit<Change, 'submodule'> & { submodule?: true }

export type Repo = {
	// From the folder: '' for the repository the folder is in.
	path: string
	name: string
	kind: RepoKind
	// The repository naming a submodule.
	parent?: string
	// A submodule named but not cloned.
	uninitialised?: true
	// Why git could not say what it holds.
	error?: string
	branch?: string
	ahead?: number
	behind?: number
	changes: RepoChange[]
}

// Statuses is every repository in the folder, and all their changes
// together.
export type Statuses = { repo: boolean; changes: RepoChange[]; repos: Repo[] }

// Past these, a folder is a home directory or a disk, not a workspace:
// what was found is shown and the rest left.
const MAX_REPOS = 64
const SCAN_DEPTH = 3
const SCAN_DIRS = 4000
const SKIP = new Set(['node_modules', 'target', 'dist', 'vendor', '__pycache__'])

// Repos is the git repositories in a folder: the one it is in, those
// within it, its submodules and theirs. Each call goes to the repository a
// path is in, its innermost, and paths in what comes back are the folder's.
// The repositories are found again with each status, which the browser
// asks for whenever files change.
export class Repos {
	private gits = new Map<string, Git>()
	private known: Repo[] | undefined
	private listeners = new Set<() => void>()
	private watching = new Map<string, () => void>()

	constructor(private root: string) {}

	private git(abs: string): Git {
		let g = this.gits.get(abs)
		if (!g) {
			g = new Git(abs)
			this.gits.set(abs, g)
		}
		return g
	}

	private abs(path: string): string {
		return path ? join(this.root, path) : this.root
	}

	async status(): Promise<Statuses> {
		type Found = { abs: string; kind: RepoKind; parent?: string; uninitialised?: true }
		const queue: Found[] = []
		const seen = new Set<string>()
		const add = (f: Found) => {
			if (seen.has(f.abs)) return
			seen.add(f.abs)
			queue.push(f)
		}
		const top = await this.git(this.root).top()
		if (top) add({ abs: this.root, kind: 'folder' })
		else for (const abs of await scan(this.root)) add({ abs, kind: 'clone' })

		const repos: Repo[] = []
		while (queue.length > 0 && repos.length < MAX_REPOS) {
			const batch = queue.splice(0, MAX_REPOS - repos.length)
			repos.push(
				...(await Promise.all(
					batch.map(async (f): Promise<Repo> => {
						const path = relative(this.root, f.abs)
						const repo: Repo = {
							path,
							name: path || basename(top?.top ?? this.root),
							kind: f.kind,
							...(f.parent !== undefined && { parent: f.parent }),
							changes: [],
						}
						if (f.uninitialised) return { ...repo, uninitialised: true }
						const git = this.git(f.abs)
						const st = await git.status()
						if (!st.repo) return { ...repo, error: st.error ?? 'git cannot read it' }
						for (const sub of await submodules(git, f.abs)) {
							add({ abs: sub, kind: 'submodule', parent: path, ...(!(await hasGit(sub)) && { uninitialised: true }) })
						}
						const base = path ? path + '/' : ''
						for (const c of st.changes) {
							// A folder untracked whole is a repository of its own.
							if (c.index === '?' && c.path.endsWith('/')) {
								const nested = join(f.abs, c.path)
								if (await hasGit(nested)) {
									add({ abs: nested.replace(/\/$/, ''), kind: 'clone' })
									continue
								}
							}
							const { submodule, ...rest } = c
							// A submodule's own changes are its own; its parent
							// has only its commit to record, and has it in its
							// working tree only once the commit has moved.
							if (submodule && submodule[1] !== 'C') rest.worktree = '.'
							if (submodule && rest.index === '.' && rest.worktree === '.') continue
							repo.changes.push({
								...rest,
								path: base + rest.path,
								...(rest.from !== undefined && { from: base + rest.from }),
								...(submodule && { submodule: true }),
							})
						}
						return { ...repo, branch: st.branch, ahead: st.ahead, behind: st.behind }
					}),
				)),
			)
		}
		repos.sort((a, b) => (a.path === '' ? -1 : b.path === '' ? 1 : a.path.localeCompare(b.path)))
		this.known = repos
		this.watch()
		return { repo: repos.length > 0, changes: repos.flatMap((r) => r.changes), repos }
	}

	// at is the repository a path is in, the innermost, and the path from
	// the folder at which that repository's own paths start. A path in none
	// goes to the folder's own, which says it is in none. A repository's own
	// folder is in it when the repository is asked for (whole), and else is
	// in its parent, as the submodule or folder its parent sees.
	async at(path: string, whole = false): Promise<{ git: Git; base: string; abs: string }> {
		const abs = isAbsolute(path) ? resolve(path) : resolve(this.root, path)
		if (!this.known) await this.status().catch(() => {})
		let best = ''
		for (const r of this.known ?? []) {
			if (!r.path || r.uninitialised || r.path.length <= best.length) continue
			const dir = this.abs(r.path)
			if ((whole && abs === dir) || abs.startsWith(dir + '/')) best = r.path
		}
		return { git: this.git(this.abs(best)), base: best ? best + '/' : '', abs }
	}

	// onChange calls fn when what any of the repositories would report may
	// have changed.
	onChange(fn: () => void): () => void {
		this.listeners.add(fn)
		this.watch()
		return () => {
			this.listeners.delete(fn)
			this.watch()
		}
	}

	// watch follows each repository found while anyone listens, and none
	// once nobody does.
	private watch() {
		const want = new Set(
			this.listeners.size === 0
				? []
				: this.known
					? this.known.filter((r) => !r.uninitialised && !r.error).map((r) => this.abs(r.path))
					: [this.root],
		)
		for (const [abs, off] of this.watching) {
			if (!want.has(abs)) {
				off()
				this.watching.delete(abs)
			}
		}
		for (const abs of want) {
			if (!this.watching.has(abs)) {
				this.watching.set(
					abs,
					this.git(abs).onChange(() => {
						for (const fn of this.listeners) fn()
					}),
				)
			}
		}
	}
}

async function hasGit(dir: string): Promise<boolean> {
	try {
		await stat(join(dir, '.git'))
		return true
	} catch {
		return false
	}
}

// submodules is where the submodules a repository names within dir would
// be, from its .gitmodules, whether cloned or not.
async function submodules(git: Git, dir: string): Promise<string[]> {
	const where = await git.top()
	if (!where) return []
	const { top, prefix } = where
	const out = await new Promise<string>((res) =>
		execFile(
			'git',
			['config', '--file', join(top, '.gitmodules'), '-z', '--get-regexp', '^submodule\\..*\\.path$'],
			{ cwd: top, encoding: 'utf8' },
			// No .gitmodules, or none in it.
			(_err, stdout) => res(stdout ?? ''),
		),
	)
	return out
		.split('\0')
		.map((e) => e.slice(e.indexOf('\n') + 1))
		.map((p) => posix.normalize(p))
		.filter((p) => p.startsWith(prefix) && p !== prefix && !isAbsolute(p))
		.map((p) => join(dir, p.slice(prefix.length)))
}

// scan finds the repositories in a folder that is in none: each folder
// with a .git, not looked inside, nor are hidden folders or dependencies'.
export async function scan(root: string): Promise<string[]> {
	const found: string[] = []
	let dirs = [root]
	let read = 0
	for (let depth = 0; depth <= SCAN_DEPTH && dirs.length > 0; depth++) {
		const next: string[] = []
		for (const dir of dirs) {
			if (read++ >= SCAN_DIRS || found.length >= MAX_REPOS) return found
			let entries: Dirent[]
			try {
				entries = await readdir(dir, { withFileTypes: true })
			} catch {
				continue
			}
			if (dir !== root && entries.some((e) => e.name === '.git')) {
				found.push(dir)
				continue
			}
			for (const e of entries) {
				if (e.isDirectory() && !e.name.startsWith('.') && !SKIP.has(e.name)) next.push(join(dir, e.name))
			}
		}
		dirs = next
	}
	return found
}
