// hangar-code: the Code tab's service, run in the guest by the agent as the
// environment's user, on a unix socket the agent joins the browser to.
//
//   node code.mjs --socket <path> --root <dir>
//
// A connection carries frames: a four-byte big-endian length, then that many
// bytes. The first byte of a frame is its channel:
//
//   'r'  JSON requests, responses and events (see handle)
//   'y'  a shared document's Yjs sync or awareness message: the document's
//        number (u32, big-endian), then the y-protocols message
//
// The browser's WebSocket carries one frame per message; the control plane
// turns one into the other.

import { createServer, type Socket } from 'node:net'
import { existsSync, unlinkSync } from 'node:fs'
import { resolve } from 'node:path'
import { parseArgs } from 'node:util'

import { Documents } from './docs.js'
import { Files } from './files.js'
import { Git } from './git.js'
import { Search } from './search.js'
import { SettingsFile } from './settings.js'
import { Connection, type Folder } from './connection.js'

const { values } = parseArgs({
	options: {
		socket: { type: 'string' },
		root: { type: 'string' },
	},
})
if (!values.socket || !values.root) {
	console.error('usage: code.mjs --socket <path> --root <dir>')
	process.exit(2)
}
const root = resolve(values.root)
const docs = new Documents(root)

// One of each per folder anyone has open, shared by every tab that has it:
// each watches the folder only while someone listens.
const folders = new Map<string, Folder>()
const folder = (dir: string): Folder => {
	let f = folders.get(dir)
	if (!f) {
		f = { root: dir, files: new Files(dir), git: new Git(dir), search: new Search(dir), hubs: new Map() }
		folders.set(dir, f)
	}
	return f
}

if (existsSync(values.socket)) unlinkSync(values.socket)
const settings = new SettingsFile()
const server = createServer((sock: Socket) => new Connection(sock, { root, folder, docs, settings }))
server.listen(values.socket, () => console.log(`hangar-code: serving ${root} on ${values.socket}`))

for (const sig of ['SIGTERM', 'SIGINT'] as const) {
	process.on(sig, async () => {
		for (const f of folders.values()) for (const hub of f.hubs.values()) hub.kill()
		await docs.saveAll()
		process.exit(0)
	})
}
