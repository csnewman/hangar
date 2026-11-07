// Runs the same cases as codemirror.rs through @codemirror/state itself.
// Usage: node codemirror.mjs <path to @codemirror/state's index.js> < cases
import { readFileSync } from 'node:fs'

const { ChangeSet, MapMode, Text } = await import(process.argv[2])

const modes = [MapMode.Simple, MapMode.TrackDel, MapMode.TrackBefore, MapMode.TrackAfter]
const cases = JSON.parse(readFileSync(0, 'utf8'))
const out = cases.map(c => {
	const doc = Text.of(c.doc.split('\n'))
	const len = doc.length
	const of = ChangeSet.of(c.changes, len)
	const a = ChangeSet.fromJSON(c.a)
	const b = ChangeSet.fromJSON(c.b)
	const after = a.apply(doc)
	const next = ChangeSet.fromJSON(c.next)
	const positions = []
	for (let p = 0; p <= len; p++)
		for (const assoc of [-1, 1])
			for (const mode of modes) positions.push(a.mapPos(p, assoc, mode))
	return {
		of: of.toJSON(),
		apply: after.toString(),
		map: a.map(b).toJSON(),
		mapBefore: a.map(b, true).toJSON(),
		compose: a.compose(next).toJSON(),
		invert: a.invert(doc).toJSON(),
		positions,
	}
})
process.stdout.write(JSON.stringify(out))
