/* SPDX-License-Identifier: GPL-2.0 */
#ifndef _LINUX_HANGAR_SYNC_H
#define _LINUX_HANGAR_SYNC_H

#include <linux/types.h>

struct file;
struct file_lock;

#ifdef CONFIG_HANGAR_SYNC
int hangar_sync_lock(struct file *filp, struct file_lock *fl, bool wait);
void hangar_sync_changed(struct file *filp);
#else
static inline int hangar_sync_lock(struct file *filp, struct file_lock *fl, bool wait)
{
	return 0;
}
static inline void hangar_sync_changed(struct file *filp) {}
#endif

#endif /* _LINUX_HANGAR_SYNC_H */
