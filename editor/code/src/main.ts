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
import { Connection } from './connection.js'

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
const files = new Files(root)
const git = new Git(root)
const docs = new Documents(root)

if (existsSync(values.socket)) unlinkSync(values.socket)
const server = createServer((sock: Socket) => new Connection(sock, { root, files, git, docs }))
server.listen(values.socket, () => console.log(`hangar-code: serving ${root} on ${values.socket}`))

for (const sig of ['SIGTERM', 'SIGINT'] as const) {
	process.on(sig, async () => {
		await docs.saveAll()
		process.exit(0)
	})
}
