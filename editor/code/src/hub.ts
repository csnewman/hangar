import { fileURLToPath } from 'node:url'

import type { Documents, SharedDoc } from './docs.js'
import type { Files } from './files.js'
import { find, Server, type Message } from './lsp.js'

// How long a server is kept once no tab uses it, so a tab reloaded, or
// opened again soon, finds it warm.
const KEEP_MS = 5 * 60_000
// How soon after an edit the server is sent the document's text.
const SYNC_AFTER_MS = 50

// Tab is one browser tab's use of a hub: what it is sent, and being told
// the server has gone.
export type Tab = { send: (msg: Message) => void; gone: (why: string) => void }

type OpenDoc = {
	doc: SharedDoc
	language: string
	version: number
	tabs: Set<number>
	dirty: boolean
	timer?: NodeJS.Timeout
	unobserve: () => void
}

// Hub is one language server for one folder, shared by every tab that has
// files of its language open there.
//
// A language server serves one client, so the hub is that client, and the
// tabs are its users: each tab's requests are passed on under ids of the
// hub's own and their answers passed back; what the server announces goes
// to every tab. Documents are the hub's to keep the server told of: they are
// the shared documents everyone edits, so the server is sent their text as
// they change, once, whichever tab changed them, and the tabs' own document
// notifications are dropped. The server's answer to initialize is kept and
// given to each tab that asks, and the last diagnostics for each file given
// to each tab that opens it.
export class Hub {
	private server: Server
	private tabs = new Map<number, Tab>()
	private nextTab = 1
	private nextId = 1
	// Requests passed on, by the hub's id: whose, and their id.
	private waiting = new Map<string, { tab: number; id: string | number }>()
	// Requests from the server passed to a tab, by the hub's id.
	private asked = new Map<string, string | number>()
	private initialized: Promise<unknown> | null = null
	private resolveInit!: (r: unknown) => void
	private rejectInit!: (e: unknown) => void
	private docs = new Map<string, OpenDoc>()
	private diagnostics = new Map<string, Message>()
	private idle: NodeJS.Timeout | undefined
	private unwatch: () => void
	private stopped = false
	readonly command: string

	private constructor(
		readonly root: string,
		readonly language: string,
		found: Exclude<ReturnType<typeof find>, { missing: string }>,
		private documents: Documents,
		files: Files,
		private gone: (why: string) => void,
	) {
		this.command = found.spec.command
		this.server = new Server(root, found, (m) => this.fromServer(m), (why) => this.exited(why))
		this.unwatch = files.onChange((_, changed) => this.server.filesChanged(changed))
	}

	// start makes a hub for a folder's files of a language, or says why
	// there is none.
	static start(
		root: string,
		language: string,
		settings: Record<string, unknown>,
		documents: Documents,
		files: Files,
		gone: (why: string) => void,
	): Hub | { missing: string } {
		const found = find(language, settings)
		if ('missing' in found) return found
		return new Hub(root, language, found, documents, files, gone)
	}

	// attach adds a tab, returning its number and how it sends.
	attach(tab: Tab): { n: number; send: (msg: Message) => void; detach: () => void } {
		clearTimeout(this.idle)
		const n = this.nextTab++
		this.tabs.set(n, tab)
		return { n, send: (m) => this.fromTab(n, m), detach: () => this.detach(n) }
	}

	private detach(n: number) {
		if (!this.tabs.delete(n)) return
		for (const [uri, d] of this.docs) {
			if (d.tabs.delete(n) && d.tabs.size === 0) this.closeDoc(uri)
		}
		for (const [hid, w] of this.waiting) if (w.tab === n) this.waiting.delete(hid)
		if (this.tabs.size === 0) {
			clearTimeout(this.idle)
			this.idle = setTimeout(() => this.stop('no tab used it'), KEEP_MS)
		}
	}

	// kill ends the server at once, the service itself stopping.
	kill() {
		this.server.kill()
	}

	stop(why: string) {
		if (this.stopped) return
		this.stopped = true
		clearTimeout(this.idle)
		this.unwatch()
		for (const uri of [...this.docs.keys()]) this.closeDoc(uri, false)
		this.server.stop()
		this.gone(why)
		for (const t of this.tabs.values()) t.gone(why)
	}

	private exited(why: string) {
		if (this.stopped) return
		this.stopped = true
		clearTimeout(this.idle)
		this.unwatch()
		for (const d of this.docs.values()) {
			clearTimeout(d.timer)
			d.unobserve()
		}
		this.docs.clear()
		this.rejectInit?.(new Error(why))
		this.gone(why)
		for (const t of this.tabs.values()) t.gone(why)
	}

	private fromTab(n: number, m: Message) {
		const tab = this.tabs.get(n)
		if (!tab) return
		if (m.method !== undefined && m.id !== undefined) this.request(n, tab, m)
		else if (m.method !== undefined) this.notification(n, m)
		else if (m.id !== undefined) {
			// A tab answering the server.
			const id = this.asked.get(String(m.id))
			if (id === undefined) return
			this.asked.delete(String(m.id))
			this.server.send({ id, result: m.result, error: m.error })
		}
	}

	private request(n: number, tab: Tab, m: Message) {
		const id = m.id!
		if (m.method === 'initialize') {
			if (!this.initialized) {
				this.initialized = new Promise((resolve, reject) => {
					this.resolveInit = resolve
					this.rejectInit = reject
				})
				const params = { ...m.params, processId: process.pid, rootUri: fileUrl(this.root), rootPath: this.root }
				params.workspaceFolders = [{ uri: fileUrl(this.root), name: this.root.split('/').pop() || this.root }]
				this.server.send({ id: 'hub-initialize', method: 'initialize', params })
			}
			this.initialized.then(
				(result) => tab.send({ id, result }),
				(e) => tab.send({ id, error: { code: -32603, message: String(e instanceof Error ? e.message : e) } }),
			)
			return
		}
		if (m.method === 'shutdown') {
			// The server is everyone's; a tab leaving does not stop it.
			tab.send({ id, result: null })
			return
		}
		// The server sees each document as it is now before it answers.
		this.flushAll()
		const hid = `${n}:${this.nextId++}`
		this.waiting.set(hid, { tab: n, id })
		this.server.send({ id: hid, method: m.method, params: m.params })
	}

	private notification(n: number, m: Message) {
		switch (m.method) {
			case 'initialized':
			case 'exit':
			case 'textDocument/didChange':
			case 'textDocument/willSave':
				return
			case 'textDocument/didOpen':
				this.openDoc(n, m.params?.textDocument?.uri, m.params?.textDocument?.languageId)
				return
			case 'textDocument/didClose': {
				const uri = m.params?.textDocument?.uri
				const d = this.docs.get(uri)
				if (d && d.tabs.delete(n) && d.tabs.size === 0) this.closeDoc(uri)
				return
			}
			case '$/cancelRequest': {
				const id = m.params?.id
				for (const [hid, w] of this.waiting) {
					if (w.tab === n && w.id === id) this.server.send({ method: '$/cancelRequest', params: { id: hid } })
				}
				return
			}
			default:
				this.server.send({ method: m.method, params: m.params })
		}
	}

	private fromServer(m: Message) {
		if (m.method !== undefined && m.id !== undefined) {
			// A request of the client's: put to a tab, the latest to attach.
			const n = [...this.tabs.keys()].pop()
			if (n === undefined) {
				this.server.send({ id: m.id, error: { code: -32601, message: 'no client is attached' } })
				return
			}
			const hid = `server:${this.nextId++}`
			this.asked.set(hid, m.id)
			this.tabs.get(n)!.send({ id: hid, method: m.method, params: m.params })
		} else if (m.method !== undefined) {
			if (m.method === 'textDocument/publishDiagnostics' && m.params?.uri) this.diagnostics.set(m.params.uri, m)
			for (const tab of this.tabs.values()) tab.send(m)
		} else if (m.id === 'hub-initialize') {
			if (m.error) this.rejectInit(new Error((m.error as { message?: string }).message ?? 'initialize failed'))
			else {
				this.server.send({ method: 'initialized', params: {} })
				this.resolveInit(m.result)
			}
		} else if (m.id !== undefined) {
			const w = this.waiting.get(String(m.id))
			if (!w) return
			this.waiting.delete(String(m.id))
			this.tabs.get(w.tab)?.send({ id: w.id, result: m.result, error: m.error })
		}
	}

	private async openDoc(n: number, uri: unknown, languageId: unknown) {
		if (typeof uri !== 'string' || !uri.startsWith('file:')) return
		const have = this.docs.get(uri)
		if (have) {
			have.tabs.add(n)
		} else {
			let doc: SharedDoc
			try {
				doc = await this.documents.open(fileURLToPath(uri))
			} catch {
				return
			}
			if (this.stopped || !this.tabs.has(n)) return
			const again = this.docs.get(uri)
			if (again) again.tabs.add(n)
			else {
				const d: OpenDoc = {
					doc,
					language: typeof languageId === 'string' ? languageId : this.language,
					version: 1,
					tabs: new Set([n]),
					dirty: false,
					unobserve: () => {},
				}
				const changed = () => {
					d.dirty = true
					clearTimeout(d.timer)
					d.timer = setTimeout(() => this.flush(uri), SYNC_AFTER_MS)
				}
				doc.text.observe(changed)
				d.unobserve = () => doc.text.unobserve(changed)
				this.docs.set(uri, d)
				await this.initialized
				if (this.docs.get(uri) !== d) return
				this.server.send({
					method: 'textDocument/didOpen',
					params: { textDocument: { uri, languageId: d.language, version: d.version, text: doc.text.toString() } },
				})
			}
		}
		const diag = this.diagnostics.get(uri)
		if (diag) this.tabs.get(n)?.send(diag)
	}

	// flush sends a document's text, if it changed since it was last sent.
	private flush(uri: string) {
		const d = this.docs.get(uri)
		if (!d || !d.dirty) return
		clearTimeout(d.timer)
		d.dirty = false
		d.version++
		this.server.send({
			method: 'textDocument/didChange',
			params: { textDocument: { uri, version: d.version }, contentChanges: [{ text: d.doc.text.toString() }] },
		})
	}

	private flushAll() {
		for (const uri of this.docs.keys()) this.flush(uri)
	}

	private closeDoc(uri: string, tell = true) {
		const d = this.docs.get(uri)
		if (!d) return
		this.docs.delete(uri)
		clearTimeout(d.timer)
		d.unobserve()
		if (tell) this.server.send({ method: 'textDocument/didClose', params: { textDocument: { uri } } })
	}
}

function fileUrl(path: string) {
	return 'file://' + path.split('/').map(encodeURIComponent).join('/')
}
