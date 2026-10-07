/* SPDX-License-Identifier: GPL-2.0 */
/*
 * hangarfs: a thin stacking filesystem over another filesystem's directory,
 * passing every operation through to it. It is Hangar's environments' root,
 * mounted over the overlayfs that merges their image and writable layer, so
 * that what Hangar needs of the files is in its own operations rather than
 * patched into the kernel's: the names an environment shares with others
 * are routed to the shared directory, an NFS mount, instead (route.c).
 *
 * Names and attributes are looked up and changed in the lower filesystem
 * with the caller's credentials. A regular file's contents are the lower
 * file's, read, written and mapped through it (fs/backing-file.c, as
 * overlayfs does), so there is one page cache, the lower one's.
 */
#ifndef _FS_HANGAR_HANGARFS_H
#define _FS_HANGAR_HANGARFS_H

#include <linux/fs.h>
#include <linux/fs_stack.h>
#include <linux/mutex.h>
#include <linux/path.h>
#include <linux/rcupdate.h>
#include <uapi/linux/hangarfs.h>

#define HANGARFS_SUPER_MAGIC 0x48414e47 /* "HANG" */

struct hfs_route {
	const char *path;	/* in the mount, from its root */
	const char *target;	/* under the shared directory */
	unsigned int len;	/* of path */
	bool dir;
	bool lock;
};

struct hfs_routes {
	struct rcu_head rcu;
	unsigned int n;
	struct hfs_route r[];	/* then the strings they point into */
};

struct hfs_sb_info {
	/* The lower directory, held for the mount's life. */
	struct path lower;
	/* What routes lead into, set with the first, then held. */
	struct path shared;
	struct mutex routes_mutex;
	struct hfs_routes __rcu *routes;
	/* Changes whenever a cached name may be routed differently. */
	atomic_long_t routes_gen;
};

/* Where a name is served from (hfs_route). */
enum hfs_route_kind {
	HFS_LOCAL,	/* the lower directory */
	HFS_ROUTED,	/* a route's target */
	HFS_LOCK,	/* a route's target, a lock: looked up there every time */
	HFS_UNDER,	/* within a routed directory, by its parent */
};

/*
 * A dentry's d_time: the generation of the routes it was looked up under,
 * and whether it is a lock.
 */
static inline unsigned long hfs_stamp(unsigned long gen, bool lock)
{
	return gen << 1 | lock;
}

struct hfs_inode_info {
	struct inode *lower;
	struct inode vfs_inode;
};

/* An open file's lower file. */
struct hfs_file {
	struct file *lower;
	/*
	 * A directory holding route points is listed whole and kept, its
	 * routed names from their targets (file.c): position is an index.
	 */
	struct hfs_dirent **entries;
	unsigned int nentries;
	char *routed;		/* hfs_route_children's */
	size_t routed_len;
};

static inline struct hfs_sb_info *HFS_SB(struct super_block *sb)
{
	return sb->s_fs_info;
}

static inline struct hfs_inode_info *HFS_I(struct inode *inode)
{
	return container_of(inode, struct hfs_inode_info, vfs_inode);
}

static inline struct inode *hfs_lower_inode(struct inode *inode)
{
	return HFS_I(inode)->lower;
}

static inline struct dentry *hfs_lower_dentry(struct dentry *dentry)
{
	return dentry->d_fsdata;
}

/* Whether a dentry's lower one is in the shared directory's filesystem. */
static inline bool hfs_is_shared(struct dentry *dentry)
{
	return hfs_lower_dentry(dentry)->d_sb !=
		HFS_SB(dentry->d_sb)->lower.dentry->d_sb;
}

static inline void hfs_lower_path(struct dentry *dentry, struct path *path)
{
	struct hfs_sb_info *sbi = HFS_SB(dentry->d_sb);

	path->dentry = hfs_lower_dentry(dentry);
	path->mnt = hfs_is_shared(dentry) ? sbi->shared.mnt : sbi->lower.mnt;
}

static inline struct file *hfs_lower_file(struct file *file)
{
	return ((struct hfs_file *)file->private_data)->lower;
}

/*
 * The inode flags the VFS checks on this filesystem's inode for what the
 * lower one allows -- writing to an immutable file, truncating an
 * append-only one -- and those that change how it is written: copied up, as
 * overlayfs does, since hfs_permission asks the lower filesystem only its
 * own question.
 */
#define HFS_COPY_I_FLAGS (S_APPEND | S_IMMUTABLE | S_SYNC | S_NOATIME | S_CASEFOLD)

/* Brings an inode's attributes and flags in step with the lower one's. */
static inline void hfs_copy_attr(struct inode *inode, struct inode *lower)
{
	fsstack_copy_attr_all(inode, lower);
	inode_set_flags(inode, lower->i_flags & HFS_COPY_I_FLAGS, HFS_COPY_I_FLAGS);
}

extern const struct inode_operations hfs_dir_iops;
extern const struct inode_operations hfs_file_iops;
extern const struct inode_operations hfs_symlink_iops;
extern const struct file_operations hfs_file_fops;
extern const struct file_operations hfs_dir_fops;
extern const struct dentry_operations hfs_dops;
extern const struct xattr_handler * const hfs_xattr_handlers[];

struct inode *hfs_iget(struct super_block *sb, struct inode *lower);

long hfs_set_routes(struct file *file, struct hangarfs_routes __user *uarg);
void hfs_free_routes(struct hfs_sb_info *sbi);
int hfs_route(const struct dentry *dentry, char *target);
bool hfs_routes_cover(const struct dentry *dentry);
struct dentry *hfs_route_lookup(struct super_block *sb, char *target);
int hfs_route_children(const struct dentry *dir, char **out, size_t *out_len);
int hfs_tmpfile(struct mnt_idmap *idmap, struct inode *dir, struct file *file,
		umode_t mode);

#endif /* _FS_HANGAR_HANGARFS_H */
