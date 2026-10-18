import { constants } from 'node:fs'
import { access, stat } from 'node:fs/promises'
import type { Socket } from 'node:net'
import { isAbsolute, resolve } from 'node:path'

import type { Documents, SharedDoc } from './docs.js'
import { ENCODINGS, type Clash, type Encoding, type Separator } from './fileops.js'
import type { Files } from './files.js'
import type { Action, Docker } from './docker.js'
import { Hub } from './hub.js'
import { find, type Message } from './lsp.js'
import type { Preview } from './preview.js'
import type { Git } from './git.js'
import type { Repos } from './repos.js'
import type { Search } from './search.js'
import { settingsPath, type SettingsFile } from './settings.js'

const CHANNEL_JSON = 0x72 // 'r'
const CHANNEL_YJS = 0x79 // 'y'
const CHANNEL_LSP = 0x6c // 'l'
const MAX_FRAME = 64 << 20

// Folder is what serves one folder: its files, its git repositories,
// searches in it, and its language servers, by language, shared by every
// tab.
export type Folder = { root: string; files: Files; repos: Repos; search: Search; hubs: Map<string, Hub> }

type Named = { path: string; from?: string }

// Services are the service's own: the folder it was started on, the one
// serving any other folder, and the shared documents, which are everyone's
// whatever folder they opened them from.
type Services = {
	root: string
	folder: (root: string) => Folder
	docs: Documents
	settings: SettingsFile
	docker: Docker
	// preview serves files over HTTP for an HTML file's preview.
	preview: Preview
}

type Request = { id: number; method: string; params?: Record<string, unknown> }

// Connection is one browser tab's Code view: its requests, the file and git
// events it is sent, and the shared documents it has open. It works in the
// service's folder unless its hello names another, for that tab alone.
export class Connection {
	private buf: Buffer = Buffer.alloc(0)
	private open = new Map<number, SharedDoc>()
	private nextDoc = 1
	private closed = false
	private f: Folder
	private unwatch: () => void = () => {}
	// The language servers this tab uses, by its number for each.
	private servers = new Map<number, { send: (m: Message) => void; detach: () => void }>()
	private nextServer = 1
	// Docker: watched once this tab asks of it, and the logs it follows.
	private unwatchDocker: (() => void) | undefined
	private logs = new Map<number, () => void>()
	private nextLog = 1

	constructor(
		private sock: Socket,
		private s: Services,
	) {
		sock.on('data', (chunk) => this.receive(chunk))
		sock.on('close', () => this.close())
		sock.on('error', () => this.close())
		this.f = s.folder(s.root)
		this.watch()
		const offSettings = s.settings.onChange((v) => this.event('settings.changed', v))
		sock.on('close', offSettings)
	}

	// watch announces every change under the folder, and every change to
	// what git reports; the browser decides what to reload.
	private watch() {
		this.unwatch()
		const offFiles = this.f.files.onChange((dirs) => this.event('fs.changed', { dirs }))
		const offGit = this.f.repos.onChange(() => this.event('git.changed', {}))
		this.unwatch = () => {
			offFiles()
			offGit()
		}
	}

	// hello starts a connection in a folder: the one asked for, which must
	// be an absolute path to a directory, or the service's own.
	private async hello(root: unknown) {
		let want = this.s.root
		if (typeof root === 'string' && root !== '') {
			if (!isAbsolute(root)) throw new Error(`${root} is not an absolute path`)
			want = resolve(root)
			let st
			try {
				st = await stat(want)
			} catch {
				throw new Error(`${want} does not exist`)
			}
			if (!st.isDirectory()) throw new Error(`${want} is not a folder`)
		}
		if (want !== this.f.root) {
			this.f = this.s.folder(want)
			this.watch()
			// A server knows one folder.
			this.stopServers()
		}
		return { root: this.f.root, home: this.s.root, settings: settingsPath }
	}

	private receive(chunk: Buffer) {
		this.buf = this.buf.length ? Buffer.concat([this.buf, chunk]) : chunk
		while (this.buf.length >= 4) {
			const len = this.buf.readUInt32BE(0)
			if (len > MAX_FRAME) {
				this.sock.destroy()
				return
			}
			if (this.buf.length < 4 + len) break
			const frame = this.buf.subarray(4, 4 + len)
			this.buf = this.buf.subarray(4 + len)
			this.frame(frame)
		}
	}

	private frame(frame: Buffer) {
		if (frame.length === 0) return
		if (frame[0] === CHANNEL_JSON) {
			let req: Request
			try {
				req = JSON.parse(frame.subarray(1).toString('utf8'))
			} catch {
				return
			}
			this.handle(req)
		} else if (frame[0] === CHANNEL_YJS && frame.length >= 5) {
			const n = frame.readUInt32BE(1)
			this.open.get(n)?.receive(this, n, frame.subarray(5))
		} else if (frame[0] === CHANNEL_LSP && frame.length >= 5) {
			const server = this.servers.get(frame.readUInt32BE(1))
			if (!server) return
			try {
				server.send(JSON.parse(frame.subarray(5).toString('utf8')))
			} catch {
				// Not JSON; nothing to pass on.
			}
		}
	}

	private async handle(req: Request) {
		try {
			const result = await this.call(req.method, req.params ?? {})
			this.json({ id: req.id, result: result ?? null })
		} catch (e) {
			this.json({ id: req.id, error: e instanceof Error ? e.message : String(e) })
		}
	}

	private call(method: string, p: Record<string, unknown>): unknown {
		const str = (k: string) => {
			const v = p[k]
			if (typeof v !== 'string') throw new Error(`${k} is required`)
			return v
		}
		const strs = (k: string) => {
			const v = p[k]
			if (!Array.isArray(v) || !v.every((x) => typeof x === 'string')) throw new Error(`${k} is required`)
			return v as string[]
		}
		const opt = (k: string) => (typeof p[k] === 'string' && p[k] ? (p[k] as string) : undefined)
		const flag = (k: string) => (typeof p[k] === 'boolean' ? (p[k] as boolean) : undefined)
		switch (method) {
			case 'hello':
				return this.hello(p.root)
			// Answered at once, so the browser can time the way here and back.
			case 'ping':
				return null
			case 'fs.list':
				return this.list(str('path'))
			case 'fs.read':
				return this.f.files.read(str('path'))
			case 'fs.patch':
				return this.f.files.patch(
					str('path'),
					str('version'),
					Array.isArray(p.runs)
						? (p.runs as { offset: unknown; data: unknown }[]).map((r) => ({
								offset: Number(r.offset),
								data: String(r.data),
							}))
						: [],
				)
			case 'fs.probe':
				return this.f.files.probe(str('path'))
			case 'fs.bytes':
				return this.f.files.bytes(
					str('path'),
					typeof p.offset === 'number' ? p.offset : 0,
					typeof p.length === 'number' ? p.length : 65536,
				)
			case 'fs.create':
				return this.f.files.create(str('path'))
			case 'fs.mkdir':
				return this.f.files.mkdir(str('path'))
			case 'fs.rename':
				return this.f.files.rename(str('from'), str('to'))
			case 'fs.delete':
				return this.f.files.remove(str('path'))
			case 'fs.transfer': {
				const op = str('op')
				const clash = str('clash')
				if (op !== 'copy' && op !== 'move') throw new Error(`no such operation: ${op}`)
				if (!['ask', 'replace', 'skip', 'rename'].includes(clash)) throw new Error(`no such choice: ${clash}`)
				return this.f.files.transfer(op, strs('from'), str('to'), clash as Clash, opt('name'))
			}
			case 'fs.trash':
				return this.f.files.trash(strs('paths'))
			case 'fs.restore': {
				const items = Array.isArray(p.items) ? (p.items as { path: unknown; id: unknown }[]) : []
				return this.f.files.restore(items.map((t) => ({ path: String(t.path), id: String(t.id) })))
			}
			case 'fs.uploadStart':
				return this.f.files.uploadStart(str('path'))
			case 'fs.uploadWrite':
				return this.f.files.uploadWrite(str('token'), str('data'))
			case 'fs.uploadEnd':
				return this.f.files.uploadEnd(str('token'), p.replace === true)
			case 'fs.uploadAbort':
				return this.f.files.uploadAbort(str('token'))
			case 'fs.zip':
				return this.f.files.zip(strs('paths'))
			case 'fs.unzip':
				return this.f.files.unzip(str('path'))
			case 'fs.setMode':
				return this.f.files.setMode(str('path'), { exec: flag('exec'), writable: flag('writable') })
			case 'fs.textProps':
				return this.f.files.textProps(str('path'))
			case 'fs.convert': {
				const separator = opt('separator')
				const encoding = opt('encoding')
				if (separator && !['LF', 'CRLF', 'CR'].includes(separator)) throw new Error(`no such separator: ${separator}`)
				if (encoding && !ENCODINGS.includes(encoding as Encoding)) throw new Error(`no such encoding: ${encoding}`)
				return this.f.files.convert(str('path'), {
					separator: separator as Separator | undefined,
					encoding: encoding as Encoding | undefined,
					bom: flag('bom'),
				})
			}
			case 'lsp.start':
				return this.startServer(str('language'))
			case 'lsp.stop':
				return this.stopServer(Number(p.server))
			case 'lsp.info':
				return this.serverInfo(str('language'))
			case 'lsp.restart': {
				// Every tab using it is told it stopped, and starts it again.
				this.f.hubs.get(str('language'))?.stop('restarted')
				return null
			}
			case 'docker.overview':
				this.unwatchDocker ??= this.s.docker.onChange(() => this.event('docker.changed', {}))
				return this.s.docker.overview()
			case 'docker.act': {
				const action = str('action')
				if (!['start', 'stop', 'restart', 'pause', 'unpause', 'remove'].includes(action)) throw new Error(`no such action: ${action}`)
				return this.s.docker.act(str('id'), action as Action)
			}
			case 'docker.project': {
				const action = str('action')
				if (!['start', 'stop', 'restart', 'down'].includes(action)) throw new Error(`no such action: ${action}`)
				return this.s.docker.project(str('project'), action as 'start' | 'stop' | 'restart' | 'down')
			}
			case 'docker.removeImage':
				return this.s.docker.removeImage(str('id'))
			case 'docker.logs': {
				const n = this.nextLog++
				const stop = this.s.docker.logs(
					str('id'),
					typeof p.tail === 'number' ? p.tail : 1000,
					(text) => this.event('docker.log', { stream: n, text }),
					(why) => {
						if (this.logs.delete(n)) this.event('docker.logEnd', { stream: n, why })
					},
				)
				this.logs.set(n, stop)
				return { stream: n }
			}
			case 'docker.logsStop': {
				const n = Number(p.stream)
				const stop = this.logs.get(n)
				this.logs.delete(n)
				stop?.()
				return
			}
			case 'preview.start':
				return this.s.preview.start()
			case 'settings.get':
				return this.s.settings.get()
			case 'settings.set':
				return this.s.settings.set(p.settings)
			case 'search.files':
				return this.f.search.files()
			case 'search.text':
				return this.f.search.text(this.textQuery(p))
			case 'search.replace':
				return this.f.search.replace(this.textQuery(p), str('replacement'), Array.isArray(p.paths) ? strs('paths') : undefined)
			case 'git.status':
				return this.f.repos.status()
			// These act on a file or folder, in the repository it is in.
			case 'git.diff': {
				const from = opt('from')
				return this.inRepo(str('path'), async (git, abs, base) =>
					named(base, await git.diff(abs, opt('against') ?? 'HEAD', from && this.abs(from), opt('to'))),
				)
			}
			case 'git.log': {
				const path = opt('path')
				const log = (git: Git, abs?: string) =>
					git.log(opt('ref'), typeof p.skip === 'number' ? p.skip : 0, typeof p.max === 'number' ? p.max : 300, abs)
				return path ? this.inRepo(path, log) : this.ofRepo(opt('repo'), (git) => log(git))
			}
			case 'git.ignore':
				return this.inRepo(str('path'), (git, abs) => git.ignore(abs, p.dir === true))
			case 'git.blob':
				return this.inRepo(str('path'), (git, abs) => git.blob(str('rev'), abs))
			case 'git.blame':
				return this.inRepo(str('path'), (git, abs) => git.blame(abs))
			case 'git.stage':
				return this.inRepo(str('path'), (git, abs) => git.stage(abs))
			case 'git.unstage':
				return this.inRepo(str('path'), (git, abs) => git.unstage(abs))
			case 'git.rollback':
				return this.inRepo(str('path'), (git, abs) => git.rollback(abs))
			case 'git.show':
				return this.inRepo(str('path'), (git, abs) => git.show(abs, p.from === 'HEAD' ? 'HEAD' : 'index'))
			case 'git.setIndex':
				return this.inRepo(str('path'), (git, abs) => git.setIndex(abs, str('content')))
			case 'git.initSubmodule':
				return this.inRepo(str('path'), (git, abs) => git.initSubmodule(abs))
			// The rest act on a whole repository: repo is a path in it, the
			// folder's own repository when none is given.
			case 'git.commitInfo':
				return this.ofRepo(opt('repo'), async (git, base) => {
					const info = await git.commitInfo(str('hash'))
					return { ...info, files: info.files.map((f) => named(base, f)) }
				})
			case 'git.branchChanges':
				return this.ofRepo(opt('repo'), async (git, base) => {
					const changes = await git.branchChanges(typeof p.base === 'string' ? p.base : undefined)
					return { ...changes, changes: changes.changes.map((c) => named(base, c)) }
				})
			case 'git.commit':
				return this.ofRepo(opt('repo'), (git) => git.commit(str('message'), p.amend === true))
			case 'git.lastMessage':
				return this.ofRepo(opt('repo'), (git) => git.lastMessage())
			case 'git.branches':
				return this.ofRepo(opt('repo'), (git) => git.branches())
			case 'git.checkout':
				return this.ofRepo(opt('repo'), (git) => git.checkout(str('name')))
			case 'git.createBranch':
				return this.ofRepo(opt('repo'), (git) => git.createBranch(str('name')))
			case 'git.push':
				return this.ofRepo(opt('repo'), (git) => git.push())
			case 'git.pull':
				return this.ofRepo(opt('repo'), (git) => git.pull())
			case 'git.fetch':
				return this.ofRepo(opt('repo'), (git) => git.fetch())
			case 'doc.open':
				return this.openDoc(str('path'))
			case 'doc.close':
				return this.closeDoc(Number(p.doc))
			default:
				throw new Error(`no such method: ${method}`)
		}
	}

	private abs(path: string): string {
		return resolve(this.f.root, path)
	}

	// inRepo runs fn on the repository a path is in, with the path made
	// absolute and where that repository's paths start in the folder.
	private async inRepo<T>(path: string, fn: (git: Git, abs: string, base: string) => Promise<T>): Promise<T> {
		const { git, abs, base } = await this.f.repos.at(path)
		return fn(git, abs, base)
	}

	// ofRepo runs fn on a whole repository, named by a path in it: the
	// folder's own when none is given.
	private async ofRepo<T>(path: string | undefined, fn: (git: Git, base: string) => Promise<T>): Promise<T> {
		const { git, base } = await this.f.repos.at(path ?? '', true)
		return fn(git, base)
	}

	private textQuery(p: Record<string, unknown>) {
		if (typeof p.query !== 'string') throw new Error('query is required')
		return {
			query: p.query,
			regex: p.regex === true,
			caseSensitive: p.caseSensitive === true,
			wholeWord: p.wholeWord === true,
			mask: typeof p.mask === 'string' ? p.mask : undefined,
			dir: typeof p.dir === 'string' && p.dir ? p.dir : undefined,
		}
	}

	// list is a folder's entries, with which of them git ignores.
	private async list(path: string) {
		const entries = await this.f.files.list(path)
		const dir = this.f.files.abs(path)
		const ignored = await this.ofRepo(dir, (git) => git.ignored(entries.map((e) => resolve(dir, e.name)))).catch(
			() => new Set<string>(),
		)
		for (const e of entries) if (ignored.has(resolve(dir, e.name))) e.ignored = true
		return entries
	}

	// startServer joins this tab to its folder's language server for a
	// language, starting it if no tab has, its messages relayed on the LSP
	// channel under the number returned. A language with no server installed
	// is told why.
	private async startServer(language: string) {
		const f = this.f
		let hub = f.hubs.get(language)
		if (!hub) {
			const started = Hub.start(f.root, language, await this.s.settings.get(), this.s.docs, f.files, () => {
				if (f.hubs.get(language) === started) f.hubs.delete(language)
			})
			if ('missing' in started) return started
			// Another request may have started one meanwhile.
			const raced = f.hubs.get(language)
			if (raced) started.stop('started twice')
			else f.hubs.set(language, started)
			hub = f.hubs.get(language)!
		}
		const n = this.nextServer++
		const tab = hub.attach({
			send: (m) => {
				const hdr = Buffer.alloc(5)
				hdr[0] = CHANNEL_LSP
				hdr.writeUInt32BE(n, 1)
				this.write(Buffer.concat([hdr, Buffer.from(JSON.stringify({ jsonrpc: '2.0', ...m }), 'utf8')]))
			},
			gone: (why) => {
				if (this.servers.delete(n)) this.event('lsp.exited', { server: n, why })
			},
		})
		this.servers.set(n, tab)
		return { server: n, root: f.root, command: hub.command }
	}

	// serverInfo is what the folder's server for a language is doing, or
	// why there is none and how one would be installed.
	private async serverInfo(language: string) {
		const hub = this.f.hubs.get(language)
		if (hub) return { running: true, ...hub.info() }
		const found = find(language, await this.s.settings.get())
		if ('missing' in found) return { running: false, ...found }
		return { running: false, command: found.spec.command, path: found.path }
	}

	private stopServer(n: number) {
		const server = this.servers.get(n)
		if (!server) return
		this.servers.delete(n)
		server.detach()
	}

	// stopServers leaves this tab's servers, which stay for others, and for
	// a while after the last leaves.
	private stopServers() {
		for (const server of this.servers.values()) server.detach()
		this.servers.clear()
	}

	private async openDoc(path: string) {
		const abs = resolve(this.f.root, path)
		const doc = await this.s.docs.open(abs)
		const n = this.nextDoc++
		this.open.set(n, doc)
		doc.join(this, n, abs)
		// A file the user may not write -- a library's, the system's -- is
		// shown, not edited.
		let writable = true
		try {
			await access(doc.path, constants.W_OK)
		} catch {
			writable = false
		}
		return { doc: n, path: abs, writable }
	}

	private closeDoc(n: number) {
		const doc = this.open.get(n)
		if (!doc) return
		this.open.delete(n)
		doc.leave(this, n)
	}

	// sendYjs sends a y-protocols message for the connection's document n.
	sendYjs(n: number, msg: Uint8Array) {
		const hdr = Buffer.alloc(5)
		hdr[0] = CHANNEL_YJS
		hdr.writeUInt32BE(n, 1)
		this.write(Buffer.concat([hdr, msg]))
	}

	// notify sends an event to this connection.
	notify(event: string, params: unknown) {
		this.json({ event, params })
	}

	private event(event: string, params: unknown) {
		this.notify(event, params)
	}

	private json(v: unknown) {
		this.write(Buffer.concat([Buffer.from([CHANNEL_JSON]), Buffer.from(JSON.stringify(v), 'utf8')]))
	}

	private write(frame: Buffer) {
		if (this.closed) return
		const len = Buffer.alloc(4)
		len.writeUInt32BE(frame.length, 0)
		this.sock.write(Buffer.concat([len, frame]))
	}

	private close() {
		if (this.closed) return
		this.closed = true
		this.unwatch()
		this.stopServers()
		this.unwatchDocker?.()
		for (const stop of this.logs.values()) stop()
		this.logs.clear()
		for (const [n, doc] of this.open) doc.leave(this, n)
		this.open.clear()
	}
}

// named gives a repository's paths in what it returns their names in the
// folder.
function named<T extends Named>(base: string, v: T): T {
	if (!base) return v
	return { ...v, path: base + v.path, ...(v.from !== undefined && { from: base + v.from }) }
}
