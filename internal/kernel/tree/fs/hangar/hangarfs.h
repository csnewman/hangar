/* SPDX-License-Identifier: GPL-2.0 */
/*
 * hangarfs: a thin stacking filesystem over another filesystem's directory,
 * passing every operation through to it. It is Hangar's environments' root,
 * mounted over the overlayfs that merges their image and writable layer, so
 * that what Hangar needs of the files -- locks shared between environments
 * first -- is in its own operations rather than patched into the kernel's.
 *
 * Names and attributes are looked up and changed in the lower filesystem
 * with the caller's credentials. A regular file's contents are the lower
 * file's, read, written and mapped through it (fs/backing-file.c, as
 * overlayfs does), so there is one page cache, the lower one's.
 */
#ifndef _FS_HANGAR_HANGARFS_H
#define _FS_HANGAR_HANGARFS_H

#include <linux/fs.h>
#include <linux/path.h>

#define HANGARFS_SUPER_MAGIC 0x48414e47 /* "HANG" */

struct hfs_sb_info {
	/* The lower directory, held for the mount's life. */
	struct path lower;
};

struct hfs_inode_info {
	struct inode *lower;
	struct inode vfs_inode;
};

/* An open file's lower file. */
struct hfs_file {
	struct file *lower;
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

static inline void hfs_lower_path(struct dentry *dentry, struct path *path)
{
	path->mnt = HFS_SB(dentry->d_sb)->lower.mnt;
	path->dentry = hfs_lower_dentry(dentry);
}

static inline struct file *hfs_lower_file(struct file *file)
{
	return ((struct hfs_file *)file->private_data)->lower;
}

extern const struct inode_operations hfs_dir_iops;
extern const struct inode_operations hfs_file_iops;
extern const struct inode_operations hfs_symlink_iops;
extern const struct file_operations hfs_file_fops;
extern const struct file_operations hfs_dir_fops;
extern const struct dentry_operations hfs_dops;
extern const struct xattr_handler * const hfs_xattr_handlers[];

struct inode *hfs_iget(struct super_block *sb, struct inode *lower);

#endif /* _FS_HANGAR_HANGARFS_H */
