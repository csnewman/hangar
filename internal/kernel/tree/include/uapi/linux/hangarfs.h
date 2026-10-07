/* SPDX-License-Identifier: GPL-2.0 WITH Linux-syscall-note */
/*
 * hangarfs routes: names in the mount that are served from another
 * directory, the shared one, rather than the filesystem under it. Hangar's
 * guest agent routes the files an environment shares with others -- its
 * owner's profile, its packs -- to an NFS mount of them.
 *
 * HANGARFS_IOC_ROUTES, on any file of the mount, replaces the routes with
 * those in buf: records of one kind byte, then the path in the mount and
 * the target under the shared directory, each ending in a NUL.
 *
 *	'f' a file: the name, and its lock file beside it (<name>.lock), are
 *	    the target's. A file saved by writing another and renaming it
 *	    over the name is copied beside the target and renamed there.
 *	'd' a directory: the name, and everything under it, are the target's
 *
 * A path is absolute, from the mount's root, with no trailing slash; a
 * target is relative, with no "..". The target's parent must exist for the
 * name to be found. shared_fd is the shared directory, set by the first
 * call and the same in every one after; len 0 removes every route.
 */
#ifndef _UAPI_LINUX_HANGARFS_H
#define _UAPI_LINUX_HANGARFS_H

#include <linux/ioctl.h>
#include <linux/types.h>

#define HANGARFS_ROUTE_FILE	'f'
#define HANGARFS_ROUTE_DIR	'd'

/* The most bytes of routes, and of routes, one call takes. */
#define HANGARFS_ROUTES_MAX_LEN	(256 * 1024)
#define HANGARFS_ROUTES_MAX	1024

struct hangarfs_routes {
	__s32 shared_fd;
	__u32 len;
	__u64 buf;
};

#define HANGARFS_IOC_ROUTES	_IOW('H', 1, struct hangarfs_routes)

#endif /* _UAPI_LINUX_HANGARFS_H */
