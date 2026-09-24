import type { Socket } from 'node:net'

import type { Documents, SharedDoc } from './docs.js'
import type { Files } from './files.js'
import type { Git } from './git.js'

const CHANNEL_JSON = 0x72 // 'r'
const CHANNEL_YJS = 0x79 // 'y'
const MAX_FRAME = 64 << 20

type Services = { root: string; files: Files; git: Git; docs: Documents }

type Request = { id: number; method: string; params?: Record<string, unknown> }

// Connection is one browser tab's Code view: its requests, the file and git
// events it is sent, and the shared documents it has open.
export class Connection {
	private buf: Buffer = Buffer.alloc(0)
	private open = new Map<number, SharedDoc>()
	private nextDoc = 1
	private closed = false
	private unwatch: () => void

	constructor(
		private sock: Socket,
		private s: Services,
	) {
		sock.on('data', (chunk) => this.receive(chunk))
		sock.on('close', () => this.close())
		sock.on('error', () => this.close())
		// Every change under the root, and every change to what git reports,
		// is announced; the browser decides what to reload.
		const offFiles = s.files.onChange((dirs) => this.event('fs.changed', { dirs }))
		const offGit = s.git.onChange(() => this.event('git.changed', {}))
		this.unwatch = () => {
			offFiles()
			offGit()
		}
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
				return { root: this.s.root }
			case 'fs.list':
				return this.s.files.list(str('path'))
			case 'fs.create':
				return this.s.files.create(str('path'))
			case 'fs.mkdir':
				return this.s.files.mkdir(str('path'))
			case 'fs.rename':
				return this.s.files.rename(str('from'), str('to'))
			case 'fs.delete':
				return this.s.files.remove(str('path'))
			case 'git.status':
				return this.s.git.status()
			case 'git.diff':
				return this.s.git.diff(str('path'))
			case 'git.stage':
				return this.s.git.stage(str('path'))
			case 'git.unstage':
				return this.s.git.unstage(str('path'))
			case 'git.rollback':
				return this.s.git.rollback(str('path'))
			case 'git.commit':
				return this.s.git.commit(str('message'))
			case 'doc.open':
				return this.openDoc(str('path'))
			case 'doc.close':
				return this.closeDoc(Number(p.doc))
			default:
				throw new Error(`no such method: ${method}`)
		}
	}

	private async openDoc(path: string) {
		const doc = await this.s.docs.open(path)
		const n = this.nextDoc++
		this.open.set(n, doc)
		doc.join(this, n)
		return { doc: n, path: doc.path }
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

	private event(event: string, params: unknown) {
		this.json({ event, params })
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
		for (const doc of this.open.values()) doc.leave(this)
		this.open.clear()
	}
}
