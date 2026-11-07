// Runs the same cases as codemirror.rs through CodeMirror itself.
// Usage: node codemirror.mjs <node_modules> < {"sets": [...], "collab": [...]}
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'

const load = name => import(pathToFileURL(join(process.argv[2], name, 'dist/index.js')))
const { ChangeSet, EditorState, MapMode, Text } = await load('@codemirror/state')
const { collab, getSyncedVersion, rebaseUpdates, receiveUpdates, sendableUpdates } =
	await load('@codemirror/collab')

const modes = [MapMode.Simple, MapMode.TrackDel, MapMode.TrackBefore, MapMode.TrackAfter]
const input = JSON.parse(readFileSync(0, 'utf8'))

const sets = input.sets.map(c => {
	const doc = Text.of(c.doc.split('\n'))
	const len = doc.length
	const a = ChangeSet.fromJSON(c.a)
	const b = ChangeSet.fromJSON(c.b)
	const positions = []
	for (let p = 0; p <= len; p++)
		for (const assoc of [-1, 1])
			for (const mode of modes) positions.push(a.mapPos(p, assoc, mode))
	return {
		of: ChangeSet.of(c.changes, len).toJSON(),
		apply: a.apply(doc).toString(),
		map: a.map(b).toJSON(),
		mapBefore: a.map(b, true).toJSON(),
		compose: a.compose(ChangeSet.fromJSON(c.next)).toJSON(),
		invert: a.invert(doc).toJSON(),
		positions,
	}
})

const updates = list => list.map(u => ({ changes: ChangeSet.fromJSON(u.changes), clientID: u.client }))

// Updates are handed over one at a time, as the Code service sends them:
// mapping over several composed is not always mapping over each in turn.
const collabs = input.collab.map(c => {
	const rebased = c.pushes.map(p => {
		let pushed = updates(p.pushed.map(changes => ({ changes, client: c.me })))
		for (const u of updates(p.over)) pushed = rebaseUpdates(pushed, [u])
		return pushed.map(u => u.changes.toJSON())
	})
	let state = EditorState.create({
		doc: c.doc,
		extensions: collab({ startVersion: c.version, clientID: c.me }),
	})
	for (const changes of c.local)
		state = state.update({ changes: ChangeSet.fromJSON(changes) }).state
	let changes = ChangeSet.empty(state.doc.length)
	for (const u of updates(c.received)) {
		const tr = receiveUpdates(state, [u])
		changes = changes.compose(tr.changes)
		state = tr.state
	}
	return {
		rebased,
		changes: changes.toJSON(),
		pending: sendableUpdates(state).map(u => u.changes.toJSON()),
		version: getSyncedVersion(state),
		doc: state.doc.toString(),
	}
})

process.stdout.write(JSON.stringify({ sets, collab: collabs }))
