/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * /dev/hangar-sync: the messages between the kernel and Hangar's guest agent
 * about locks on files an environment shares with others.
 *
 * Each read returns one message from the kernel and each write takes one from
 * the agent: a struct hangar_sync_msg, then path_len bytes of path with no
 * terminator. len is the whole message's length.
 */
#ifndef _UAPI_LINUX_HANGAR_SYNC_H
#define _UAPI_LINUX_HANGAR_SYNC_H

#include <linux/types.h>

/* Kernel to agent. */
/* A process here asks for a lock on the file: take it for this environment
 * and reply with its id. */
#define HANGAR_SYNC_LOCK	1
/* No process here holds a lock on the file any more. No reply. */
#define HANGAR_SYNC_IDLE	2
/* The LOCK with this id is no longer wanted: the process was interrupted.
 * No reply. */
#define HANGAR_SYNC_CANCEL	3

/* Agent to kernel. */
/* The answer to a LOCK: result is 0 if this environment holds the lock, or
 * a negative errno (-EAGAIN when another holds it and the caller would not
 * wait). */
#define HANGAR_SYNC_REPLY	16
/* Locks on files at or under path are this environment's to share. */
#define HANGAR_SYNC_ADD_PATH	17
/* No path is shared any more. */
#define HANGAR_SYNC_CLEAR_PATHS	18

/* flags on a LOCK. */
#define HANGAR_SYNC_WAIT	1	/* the caller waits for the lock */
#define HANGAR_SYNC_FLOCK	2	/* flock(2), not a POSIX or OFD lock */

struct hangar_sync_msg {
	__u32 len;
	__u32 op;
	__u64 id;
	__s32 result;
	__u32 type;		/* F_RDLCK or F_WRLCK */
	__u32 flags;
	__u32 pid;
	__u64 start;
	__u64 end;
	__u32 path_len;
	__u32 pad;
};

#endif /* _UAPI_LINUX_HANGAR_SYNC_H */
