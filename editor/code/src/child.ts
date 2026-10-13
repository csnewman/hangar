import * as cp from 'node:child_process'

// The agent starts this service protected from the kernel's OOM killer
// (internal/oomscore), and what the service starts would inherit that: a
// language server of several gigabytes, or a commit hook's build, would
// outlive the user's own work when memory runs out. So it starts them
// through a shell that first gives them the score of anything else the
// user runs, and then becomes them.
const UNPROTECT = '{ echo 0 >/proc/self/oom_score_adj; } 2>/dev/null; exec "$0" "$@"'

function via(file: string, args: readonly string[]): [string, string[]] {
	return process.platform === 'linux' ? ['/bin/sh', ['-c', UNPROTECT, file, ...args]] : [file, [...args]]
}

// execFile and spawn are node:child_process's, for a file and its
// arguments, started as above.
export const execFile = ((file: string, args: readonly string[], ...rest: unknown[]) =>
	(cp.execFile as (...a: unknown[]) => cp.ChildProcess)(...via(file, args), ...rest)) as typeof cp.execFile

export const spawn = ((file: string, args: readonly string[], ...rest: unknown[]) =>
	(cp.spawn as (...a: unknown[]) => cp.ChildProcess)(...via(file, args), ...rest)) as typeof cp.spawn
