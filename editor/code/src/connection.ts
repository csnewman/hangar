import { constants } from 'node:fs'
import { access, stat } from 'node:fs/promises'
import type { Socket } from 'node:net'
import { isAbsolute, resolve } from 'node:path'

import type { Documents, SharedDoc } from './docs.js'
import type { Files } from './files.js'
import type { Git } from './git.js'
import type { Action, Docker } from './docker.js'
import { Hub } from './hub.js'
import type { Message } from './lsp.js'
import type { Search } from './search.js'
import type { SettingsFile } from './settings.js'

const CHANNEL_JSON = 0x72 // 'r'
const CHANNEL_YJS = 0x79 // 'y'
const CHANNEL_LSP = 0x6c // 'l'
const MAX_FRAME = 64 << 20

// Folder is what serves one folder: its files, its git, searches in it, and
// its language servers, by language, shared by every tab.
export type Folder = { root: string; files: Files; git: Git; search: Search; hubs: Map<string, Hub> }

// Services are the service's own: the folder it was started on, the one
// serving any other folder, and the shared documents, which are everyone's
// whatever folder they opened them from.
type Services = {
	root: string
	folder: (root: string) => Folder
	docs: Documents
	settings: SettingsFile
	docker: Docker
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
		const offGit = this.f.git.onChange(() => this.event('git.changed', {}))
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
		return { root: this.f.root, home: this.s.root }
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
			const doc = this.open.get(frame.readUInt32BE(1))
			doc?.receive(this, frame.subarray(5))
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
		switch (method) {
			case 'hello':
				return this.hello(p.root)
			case 'fs.list':
				return this.f.files.list(str('path'))
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
			case 'lsp.start':
				return this.startServer(str('language'))
			case 'lsp.stop':
				return this.stopServer(Number(p.server))
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
			case 'settings.get':
				return this.s.settings.get()
			case 'settings.set':
				return this.s.settings.set(p.settings)
			case 'search.files':
				return this.f.search.files()
			case 'search.text':
				return this.f.search.text({
					query: str('query'),
					regex: p.regex === true,
					caseSensitive: p.caseSensitive === true,
					wholeWord: p.wholeWord === true,
					mask: typeof p.mask === 'string' ? p.mask : undefined,
				})
			case 'git.status':
				return this.f.git.status()
			case 'git.diff':
				return this.f.git.diff(
					str('path'),
					typeof p.against === 'string' && p.against ? p.against : 'HEAD',
					typeof p.from === 'string' && p.from ? p.from : undefined,
					typeof p.to === 'string' && p.to ? p.to : undefined,
				)
			case 'git.log':
				return this.f.git.log(
					typeof p.ref === 'string' && p.ref ? p.ref : undefined,
					typeof p.skip === 'number' ? p.skip : 0,
					typeof p.max === 'number' ? p.max : 300,
				)
			case 'git.blob':
				return this.f.git.blob(str('rev'), str('path'))
			case 'git.commitInfo':
				return this.f.git.commitInfo(str('hash'))
			case 'git.blame':
				return this.f.git.blame(str('path'))
			case 'git.branchChanges':
				return this.f.git.branchChanges(typeof p.base === 'string' ? p.base : undefined)
			case 'git.stage':
				return this.f.git.stage(str('path'))
			case 'git.unstage':
				return this.f.git.unstage(str('path'))
			case 'git.rollback':
				return this.f.git.rollback(str('path'))
			case 'git.commit':
				return this.f.git.commit(str('message'), p.amend === true)
			case 'git.lastMessage':
				return this.f.git.lastMessage()
			case 'git.show':
				return this.f.git.show(str('path'), p.from === 'HEAD' ? 'HEAD' : 'index')
			case 'git.setIndex':
				return this.f.git.setIndex(str('path'), str('content'))
			case 'git.branches':
				return this.f.git.branches()
			case 'git.checkout':
				return this.f.git.checkout(str('name'))
			case 'git.createBranch':
				return this.f.git.createBranch(str('name'))
			case 'git.push':
				return this.f.git.push()
			case 'git.pull':
				return this.f.git.pull()
			case 'git.fetch':
				return this.f.git.fetch()
			case 'doc.open':
				return this.openDoc(str('path'))
			case 'doc.close':
				return this.closeDoc(Number(p.doc))
			default:
				throw new Error(`no such method: ${method}`)
		}
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
			if ('missing' in started) return { missing: started.missing }
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
		const doc = await this.s.docs.open(resolve(this.f.root, path))
		const n = this.nextDoc++
		this.open.set(n, doc)
		doc.join(this, n)
		// A file the user may not write -- a library's, the system's -- is
		// shown, not edited.
		let writable = true
		try {
			await access(doc.path, constants.W_OK)
		} catch {
			writable = false
		}
		return { doc: n, path: doc.path, writable }
	}

	private closeDoc(n: number) {
		const doc = this.open.get(n)
		if (!doc) return
		this.open.delete(n)
		doc.leave(this)
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
		for (const doc of this.open.values()) doc.leave(this)
		this.open.clear()
	}
}
