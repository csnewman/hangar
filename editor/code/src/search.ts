import { execFile, spawn } from 'node:child_process'
import { open, readdir } from 'node:fs/promises'
import { join } from 'node:path'

// The most files listed for Go to File, and the most matches one search
// returns.
const MAX_FILES = 100_000
const MAX_MATCHES = 2000
// A match's line is cut to this many characters around the match.
const MAX_LINE = 400
// Files larger than this are not searched by the fallback search.
const MAX_SEARCH_SIZE = 2 << 20
const SEARCH_TIMEOUT_MS = 20_000

// Directories never listed or searched without git or ripgrep to say what
// is ignored: they churn, are generated, or are someone else's.
const SKIP = new Set(['.git', 'node_modules', '.cache', 'target', 'dist', '__pycache__', '.venv', 'vendor'])

export type TextQuery = {
	query: string
	regex?: boolean
	caseSensitive?: boolean
	wholeWord?: boolean
	// Comma-separated globs, as IntelliJ's file mask: "*.go, !*_test.go".
	mask?: string
}

export type Match = {
	path: string
	// 1-based.
	line: number
	text: string
	// Each match in text, as [start, end) offsets.
	ranges: [number, number][]
}

export type TextResult = { matches: Match[]; truncated: boolean }

function exec(cmd: string, args: string[], cwd: string): Promise<string> {
	return new Promise((resolve, reject) => {
		execFile(cmd, args, { cwd, maxBuffer: 256 << 20, encoding: 'utf8', timeout: SEARCH_TIMEOUT_MS }, (err, out) =>
			err ? reject(err) : resolve(out),
		)
	})
}

let rgChecked: Promise<boolean> | undefined
function haveRipgrep(): Promise<boolean> {
	rgChecked ??= new Promise((resolve) => execFile('rg', ['--version'], (err) => resolve(!err)))
	return rgChecked
}

// Search finds files by name and text in files under a root, leaving out
// what git (or ripgrep) is told to ignore.
export class Search {
	constructor(private root: string) {}

	// files lists the files under the root, relative to it: git's tracked and
	// untracked but not ignored files in a repository, ripgrep's otherwise,
	// or a walk that skips the usual generated directories.
	async files(): Promise<{ files: string[]; truncated: boolean }> {
		let list: string[] | undefined
		try {
			list = (await exec('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard'], this.root))
				.split('\0')
				.filter(Boolean)
			list = [...new Set(list)]
		} catch {
			// Not a repository.
		}
		if (!list && (await haveRipgrep())) {
			try {
				list = (await exec('rg', ['--files', '--hidden', '--glob', '!.git', '-0'], this.root)).split('\0').filter(Boolean)
			} catch {
				// Fall back to walking.
			}
		}
		list ??= await this.walk()
		list.sort()
		return list.length > MAX_FILES ? { files: list.slice(0, MAX_FILES), truncated: true } : { files: list, truncated: false }
	}

	private async walk(): Promise<string[]> {
		const out: string[] = []
		const visit = async (rel: string) => {
			if (out.length > MAX_FILES) return
			let ents
			try {
				ents = await readdir(join(this.root, rel), { withFileTypes: true })
			} catch {
				return
			}
			for (const e of ents) {
				const p = rel ? `${rel}/${e.name}` : e.name
				if (e.isDirectory()) {
					if (!SKIP.has(e.name)) await visit(p)
				} else if (e.isFile() || e.isSymbolicLink()) out.push(p)
			}
		}
		await visit('')
		return out
	}

	// text finds lines matching a query: with ripgrep where the image has
	// it, else by reading each file Go to File would list.
	async text(q: TextQuery): Promise<TextResult> {
		if (!q.query) return { matches: [], truncated: false }
		const re = toRegExp(q)
		if (await haveRipgrep()) {
			try {
				return await this.ripgrep(q, re)
			} catch {
				// Fall back to reading the files.
			}
		}
		return this.scan(q, re)
	}

	private ripgrep(q: TextQuery, re: RegExp): Promise<TextResult> {
		const args = ['--json', '--hidden', '--glob', '!.git', '--max-columns', String(MAX_LINE * 4), '--max-filesize', '8M']
		if (!q.regex) args.push('--fixed-strings')
		if (!q.caseSensitive) args.push('--ignore-case')
		if (q.wholeWord) args.push('--word-regexp')
		for (const g of globs(q.mask)) args.push('--glob', g)
		args.push('--', q.query, '.')
		return new Promise((resolve, reject) => {
			const child = spawn('rg', args, { cwd: this.root })
			const matches: Match[] = []
			let truncated = false
			let rest = ''
			const timer = setTimeout(() => {
				truncated = true
				child.kill()
			}, SEARCH_TIMEOUT_MS)
			child.stdout.setEncoding('utf8')
			child.stdout.on('data', (chunk: string) => {
				const lines = (rest + chunk).split('\n')
				rest = lines.pop() ?? ''
				for (const l of lines) {
					if (matches.length >= MAX_MATCHES) {
						truncated = true
						child.kill()
						return
					}
					let m
					try {
						m = JSON.parse(l)
					} catch {
						continue
					}
					if (m.type !== 'match') continue
					const path = String(m.data.path.text ?? '').replace(/^\.\//, '')
					const text = String(m.data.lines.text ?? '').replace(/\r?\n$/, '')
					if (!path || m.data.lines.text === undefined) continue
					matches.push(cut(path, m.data.line_number, text, re))
				}
			})
			child.on('error', (e) => {
				clearTimeout(timer)
				reject(e)
			})
			child.on('close', (code) => {
				clearTimeout(timer)
				// 1 is no matches; 2 is an error, such as a bad pattern, unless
				// some files simply could not be read.
				if (code === 2 && matches.length === 0 && !truncated) reject(new Error('ripgrep failed'))
				else resolve({ matches, truncated })
			})
		})
	}

	private async scan(q: TextQuery, re: RegExp): Promise<TextResult> {
		const { files } = await this.files()
		const include = matcher(q.mask)
		const matches: Match[] = []
		const deadline = Date.now() + SEARCH_TIMEOUT_MS
		for (const f of files) {
			if (!include(f)) continue
			if (matches.length >= MAX_MATCHES || Date.now() > deadline) return { matches, truncated: true }
			const text = await readText(join(this.root, f))
			if (text === null) continue
			const lines = text.split('\n')
			for (let i = 0; i < lines.length; i++) {
				const line = lines[i].replace(/\r$/, '')
				re.lastIndex = 0
				if (!re.test(line)) continue
				matches.push(cut(f, i + 1, line, re))
				if (matches.length >= MAX_MATCHES) return { matches, truncated: true }
			}
		}
		return { matches, truncated: false }
	}
}

// toRegExp is the query as a JavaScript expression, used to find where in
// each line it matched.
export function toRegExp(q: TextQuery): RegExp {
	let src = q.regex ? q.query : q.query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
	if (q.wholeWord) src = `\\b(?:${src})\\b`
	try {
		return new RegExp(src, q.caseSensitive ? 'g' : 'gi')
	} catch (e) {
		throw new Error(`not a valid expression: ${e instanceof Error ? e.message : String(e)}`)
	}
}

// cut is a match with its line shortened to what is around the first
// match, and the matches' places in what is left.
function cut(path: string, line: number, full: string, re: RegExp): Match {
	const ranges: [number, number][] = []
	re.lastIndex = 0
	for (let m = re.exec(full); m; m = re.exec(full)) {
		if (m[0].length === 0) {
			re.lastIndex++
			continue
		}
		ranges.push([m.index, m.index + m[0].length])
		if (ranges.length > 50) break
	}
	let text = full
	let shift = 0
	if (text.length > MAX_LINE) {
		const first = ranges[0]?.[0] ?? 0
		shift = Math.max(0, Math.min(first - 80, text.length - MAX_LINE))
		text = text.slice(shift, shift + MAX_LINE)
	}
	const lead = text.length - text.trimStart().length
	return {
		path,
		line,
		text: text.slice(lead),
		ranges: ranges
			.map(([s, e]): [number, number] => [s - shift - lead, e - shift - lead])
			.filter(([s, e]) => s >= 0 && e <= text.length - lead),
	}
}

// globs turns a file mask into ripgrep's globs: each comma-separated
// pattern, "!" leaving files out.
function globs(mask?: string): string[] {
	return (mask ?? '')
		.split(',')
		.map((g) => g.trim())
		.filter(Boolean)
}

// matcher is a file mask as a test of a path, for the fallback search.
function matcher(mask?: string): (path: string) => boolean {
	const gs = globs(mask)
	if (gs.length === 0) return () => true
	const toRe = (g: string) =>
		new RegExp(
			'(^|/)' +
				g
					.replace(/[.+^${}()|[\]\\]/g, '\\$&')
					.replace(/\*\*/g, '\0')
					.replace(/\*/g, '[^/]*')
					.replace(/\?/g, '[^/]')
					.replace(/\0/g, '.*') +
				'$',
		)
	const inc = gs.filter((g) => !g.startsWith('!')).map(toRe)
	const exc = gs.filter((g) => g.startsWith('!')).map((g) => toRe(g.slice(1)))
	return (p) => (inc.length === 0 || inc.some((r) => r.test(p))) && !exc.some((r) => r.test(p))
}

// readText is a file's text, or null for one too large or not text.
async function readText(abs: string): Promise<string | null> {
	let fh
	try {
		fh = await open(abs, 'r')
		const st = await fh.stat()
		if (!st.isFile() || st.size > MAX_SEARCH_SIZE) return null
		const buf = await fh.readFile()
		if (buf.subarray(0, 8000).includes(0)) return null
		return buf.toString('utf8')
	} catch {
		return null
	} finally {
		await fh?.close()
	}
}
