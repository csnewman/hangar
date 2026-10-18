// describe is what went wrong, as a person reads it: a file system error
// said in words with the path it is about ("/workspace/x does not exist"),
// rather than Node's "ENOENT: no such file or directory, scandir
// '/workspace/x'"; anything else as its own message.
export function describe(e: unknown): string {
	if (!(e instanceof Error)) return String(e)
	const err = e as NodeJS.ErrnoException & { dest?: string }
	const path = err.path ?? 'The file'
	switch (err.code) {
		case 'ENOENT':
			return `${path} does not exist`
		case 'EACCES':
		case 'EPERM':
			return `${path}: permission denied`
		case 'ENOTDIR':
			return `${path} is not a folder`
		case 'EISDIR':
			return `${path} is a folder`
		case 'EEXIST':
			return `${err.dest ?? path} already exists`
		case 'ENOTEMPTY':
			return `${path} is not empty`
		case 'ENOSPC':
			return 'The disk is full'
		case 'EDQUOT':
			return 'The disk quota is used up'
		case 'EROFS':
			return `${path} is on a read-only file system`
		case 'EBUSY':
			return `${path} is in use`
		case 'ELOOP':
			return `${path} is a link that leads back to itself`
		case 'ENAMETOOLONG':
			return `${path} is too long a name`
		case 'EMFILE':
		case 'ENFILE':
			return 'Too many files are open'
		case 'EXDEV':
			return `${err.dest ?? path} is on another file system`
	}
	return err.message
}
