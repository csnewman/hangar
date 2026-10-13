// What the service starts runs at the OOM score of the user's own
// processes, not at the protected one the agent gives the service.
import { writeFileSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'

import { execFile, spawn } from '../src/child.ts'

const linux = process.platform === 'linux'

function run(file: string, args: string[]): Promise<string> {
	return new Promise((resolve, reject) =>
		execFile(file, args, { encoding: 'utf8' }, (err, stdout) => (err ? reject(err) : resolve(stdout))),
	)
}

test('a child starts at the score of the user’s processes', { skip: !linux }, async () => {
	// Raising its own score needs no privilege, and stands in for the
	// service's own, which differs from what it starts.
	writeFileSync('/proc/self/oom_score_adj', '100')
	try {
		assert.equal((await run('cat', ['/proc/self/oom_score_adj'])).trim(), '0')
	} finally {
		writeFileSync('/proc/self/oom_score_adj', '0')
	}
})

test('a child gets its arguments as they are', async () => {
	const args = ['$HOME', '"q"', 'a b', '', '-n']
	assert.equal(await run('printf', ['%s|', ...args]), args.map((a) => `${a}|`).join(''))
})

test('a program that is not there fails', async () => {
	await assert.rejects(run('hangar-no-such-program', []))
	const code = await new Promise((resolve) => {
		const child = spawn('hangar-no-such-program', [])
		child.on('error', () => resolve('error'))
		child.on('exit', (c) => resolve(c))
	})
	assert.notEqual(code, 0)
})
