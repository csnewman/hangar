import { watch, type FSWatcher } from 'node:fs'
import { readFile, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, isAbsolute, resolve } from 'node:path'

import * as decoding from 'lib0/decoding'
import * as encoding from 'lib0/encoding'
import * as awarenessProtocol from 'y-protocols/awareness'
import * as syncProtocol from 'y-protocols/sync'
import * as Y from 'yjs'

import type { Connection } from './connection.js'

const MESSAGE_SYNC = 0
const MESSAGE_AWARENESS = 1

// The largest file opened for editing.
const MAX_SIZE = 8 << 20
// How long after the last edit a document is written to disk.
const SAVE_AFTER_MS = 600
// How long a document nobody has open is kept, for a window coming back.
const KEEP_MS = 60_000

// Edits that came from the file on disk, not from an editor: they are not
// written back.
const DISK = Symbol('disk')

// Documents holds the files open for editing, one shared document each,
// whoever has them open.
export class Documents {
	private docs = new Map<string, Promise<SharedDoc>>()

	constructor(private root: string) {}

	open(path: string): Promise<SharedDoc> {
		const abs = isAbsolute(path) ? resolve(path) : resolve(this.root, path)
		let p = this.docs.get(abs)
		if (!p) {
			p = SharedDoc.load(abs, () => this.docs.delete(abs))
			this.docs.set(abs, p)
			p.catch(() => this.docs.delete(abs))
		}
		return p
	}

	async saveAll() {
		for (const p of this.docs.values()) {
			try {
				await (await p).save()
			} catch {
				// The file went away; nothing to save it to.
			}
		}
	}
}

type Member = { n: number; clients: Set<number> }

// SharedDoc is one file as a Yjs document, kept in step with the file on disk
// both ways: edits are written back after a short idle, and changes to the
// file from anywhere else are merged into the document as edits.
export class SharedDoc {
	readonly doc = new Y.Doc()
	readonly text = this.doc.getText('text')
	readonly awareness = new awarenessProtocol.Awareness(this.doc)
	private members = new Map<Connection, Member>()
	private saveTimer: NodeJS.Timeout | undefined
	private dropTimer: NodeJS.Timeout | undefined
	private watcher: FSWatcher | undefined
	// What the file held when last read or written, so a change on disk
	// that is our own write is recognised.
	private onDisk = ''
	// Whether the file used CRLF line endings. The document holds LF, which
	// is what editors work in, and the file gets its own endings back.
	private crlf = false
	// Whether the file is gone from disk. A deleted file's document is not
	// saved, which would bring the file back; it follows the file again if
	// it reappears.
	private deleted = false

	private constructor(
		readonly path: string,
		private dropped: () => void,
	) {
		// No awareness state of its own: the server is not a participant.
		this.awareness.setLocalState(null)
		this.doc.on('update', (update: Uint8Array, origin: unknown) => {
			const enc = encoding.createEncoder()
			encoding.writeVarUint(enc, MESSAGE_SYNC)
			syncProtocol.writeUpdate(enc, update)
			const msg = encoding.toUint8Array(enc)
			for (const [conn, m] of this.members) conn.sendYjs(m.n, msg)
			if (origin !== DISK) this.scheduleSave()
		})
		this.awareness.on('update', ({ added, updated, removed }: AwarenessChange, origin: unknown) => {
			const changed = [...added, ...updated, ...removed]
			const from = origin instanceof Object ? this.members.get(origin as Connection) : undefined
			if (from) {
				for (const id of added) from.clients.add(id)
				for (const id of removed) from.clients.delete(id)
			}
			const enc = encoding.createEncoder()
			encoding.writeVarUint(enc, MESSAGE_AWARENESS)
			encoding.writeVarUint8Array(enc, awarenessProtocol.encodeAwarenessUpdate(this.awareness, changed))
			const msg = encoding.toUint8Array(enc)
			for (const [conn, m] of this.members) conn.sendYjs(m.n, msg)
		})
	}

	static async load(path: string, dropped: () => void): Promise<SharedDoc> {
		const st = await stat(path)
		if (!st.isFile()) throw new Error(`${basename(path)} is not a file`)
		if (st.size > MAX_SIZE) throw new Error(`${basename(path)} is too large to edit here`)
		const bytes = await readFile(path)
		if (bytes.subarray(0, 8192).includes(0)) throw new Error(`${basename(path)} is not a text file`)
		const d = new SharedDoc(path, dropped)
		const content = bytes.toString('utf8')
		d.crlf = content.includes('\r\n')
		d.onDisk = content
		d.doc.transact(() => d.text.insert(0, d.fromDisk(content)), DISK)
		d.watch()
		return d
	}

	private fromDisk(s: string) {
		return this.crlf ? s.replace(/\r\n/g, '\n') : s
	}

	private toDisk(s: string) {
		return this.crlf ? s.replace(/\n/g, '\r\n') : s
	}

	join(conn: Connection, n: number) {
		clearTimeout(this.dropTimer)
		this.members.set(conn, { n, clients: new Set() })
		// The server's state, for the new member to catch up from, and who
		// else is here.
		const enc = encoding.createEncoder()
		encoding.writeVarUint(enc, MESSAGE_SYNC)
		syncProtocol.writeSyncStep1(enc, this.doc)
		conn.sendYjs(n, encoding.toUint8Array(enc))
		const states = [...this.awareness.getStates().keys()]
		if (states.length > 0) {
			const aw = encoding.createEncoder()
			encoding.writeVarUint(aw, MESSAGE_AWARENESS)
			encoding.writeVarUint8Array(aw, awarenessProtocol.encodeAwarenessUpdate(this.awareness, states))
			conn.sendYjs(n, encoding.toUint8Array(aw))
		}
	}

	leave(conn: Connection) {
		const m = this.members.get(conn)
		if (!m) return
		this.members.delete(conn)
		awarenessProtocol.removeAwarenessStates(this.awareness, [...m.clients], null)
		if (this.members.size === 0) {
			this.dropTimer = setTimeout(() => this.drop(), KEEP_MS)
		}
	}

	receive(conn: Connection, msg: Uint8Array) {
		const m = this.members.get(conn)
		if (!m) return
		const dec = decoding.createDecoder(msg)
		const type = decoding.readVarUint(dec)
		if (type === MESSAGE_SYNC) {
			const enc = encoding.createEncoder()
			encoding.writeVarUint(enc, MESSAGE_SYNC)
			syncProtocol.readSyncMessage(dec, enc, this.doc, conn)
			if (encoding.length(enc) > 1) conn.sendYjs(m.n, encoding.toUint8Array(enc))
		} else if (type === MESSAGE_AWARENESS) {
			awarenessProtocol.applyAwarenessUpdate(this.awareness, decoding.readVarUint8Array(dec), conn)
		}
	}

	private scheduleSave() {
		clearTimeout(this.saveTimer)
		this.saveTimer = setTimeout(() => {
			this.save().catch((e) => console.error(`hangar-code: saving ${this.path}: ${e}`))
		}, SAVE_AFTER_MS)
	}

	async save() {
		clearTimeout(this.saveTimer)
		if (this.deleted) return
		const content = this.toDisk(this.text.toString())
		if (content === this.onDisk) return
		this.onDisk = content
		await writeFile(this.path, content)
	}

	// watch follows the file's directory, not the file: an editor that saves
	// by writing a new file and renaming it over the old one would end a watch
	// on the file itself.
	private watch() {
		const name = basename(this.path)
		try {
			this.watcher = watch(dirname(this.path), (_event, changed) => {
				if (changed && String(changed) === name) this.reload()
			})
			this.watcher.on('error', () => this.watcher?.close())
		} catch {
			// Without a watch, the document still saves; it just will not
			// follow changes made elsewhere.
		}
	}

	private async reload() {
		let content: string
		try {
			content = await readFile(this.path, 'utf8')
		} catch (e) {
			if ((e as NodeJS.ErrnoException).code === 'ENOENT' && !this.deleted) {
				this.deleted = true
				clearTimeout(this.saveTimer)
				for (const conn of this.members.keys()) conn.notify('doc.deleted', { path: this.path })
			}
			return
		}
		this.deleted = false
		if (content === this.onDisk) return
		this.onDisk = content
		const next = this.fromDisk(content)
		const cur = this.text.toString()
		if (next === cur) return
		// The smallest single edit that turns the document into the file: the
		// part both share at the start and the end stays, so carets and
		// anyone's typing elsewhere in the document are undisturbed.
		let start = 0
		while (start < cur.length && start < next.length && cur[start] === next[start]) start++
		let endCur = cur.length
		let endNext = next.length
		while (endCur > start && endNext > start && cur[endCur - 1] === next[endNext - 1]) {
			endCur--
			endNext--
		}
		this.doc.transact(() => {
			if (endCur > start) this.text.delete(start, endCur - start)
			if (endNext > start) this.text.insert(start, next.slice(start, endNext))
		}, DISK)
	}

	private async drop() {
		if (this.members.size > 0) return
		try {
			await this.save()
		} catch {
			// Nowhere left to save it.
		}
		this.watcher?.close()
		this.awareness.destroy()
		this.doc.destroy()
		this.dropped()
	}
}

type AwarenessChange = { added: number[]; updated: number[]; removed: number[] }
