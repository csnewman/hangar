// SPDX-License-Identifier: GPL-2.0
/*
 * hangarfs: names, attributes and extended attributes, each passed to the
 * lower filesystem. A hangarfs inode stands for one lower inode, found again
 * by it, so hard links stay one inode here as they are below.
 */

#include "hangarfs.h"
#include <linux/fileattr.h>
#include <linux/fs_stack.h>
#include <linux/namei.h>
#include <linux/posix_acl.h>
#include <linux/posix_acl_xattr.h>
#include <linux/xattr.h>

static int hfs_inode_test(struct inode *inode, void *lower)
{
	return hfs_lower_inode(inode) == lower;
}

static int hfs_inode_set(struct inode *inode, void *data)
{
	struct inode *lower = data;

	HFS_I(inode)->lower = lower;
	inode->i_ino = lower->i_ino;
	hfs_copy_attr(inode, lower);
	fsstack_copy_inode_size(inode, lower);

	if (S_ISDIR(inode->i_mode)) {
		inode->i_op = &hfs_dir_iops;
		inode->i_fop = &hfs_dir_fops;
	} else if (S_ISLNK(inode->i_mode)) {
		inode->i_op = &hfs_symlink_iops;
	} else if (S_ISREG(inode->i_mode)) {
		inode->i_op = &hfs_file_iops;
		inode->i_fop = &hfs_file_fops;
	} else {
		inode->i_op = &hfs_file_iops;
		init_special_inode(inode, inode->i_mode, lower->i_rdev);
	}
	return 0;
}

/* The inode for a lower inode, new or found again, still locked if new. */
static struct inode *__hfs_iget(struct super_block *sb, struct inode *lower)
{
	struct inode *inode;

	if (!igrab(lower))
		return ERR_PTR(-ESTALE);
	inode = iget5_locked(sb, (unsigned long)lower, hfs_inode_test,
			     hfs_inode_set, lower);
	if (!inode) {
		iput(lower);
		return ERR_PTR(-ENOMEM);
	}
	/* An inode found again holds its own reference to the lower one. */
	if (!(inode_state_read_once(inode) & I_NEW))
		iput(lower);
	return inode;
}

struct inode *hfs_iget(struct super_block *sb, struct inode *lower)
{
	struct inode *inode = __hfs_iget(sb, lower);

	if (!IS_ERR(inode) && (inode_state_read_once(inode) & I_NEW))
		unlock_new_inode(inode);
	return inode;
}

/*
 * Gives a new name the inode of the lower dentry made for it. A lower
 * filesystem may leave that dentry unhashed (overlayfs does after a hard
 * link), to be looked up afresh: then so is this one.
 */
static int hfs_interpose(struct dentry *lower, struct dentry *dentry)
{
	struct inode *inode;

	if (d_unhashed(lower)) {
		d_drop(dentry);
		return 0;
	}
	inode = hfs_iget(dentry->d_sb, d_inode(lower));

	if (IS_ERR(inode))
		return PTR_ERR(inode);
	d_instantiate(dentry, inode);
	return 0;
}

/*
 * The lower dentry for a name being made, its parent locked: the dentry
 * the lookup found, which a create replaces with the lower one it makes.
 */
static struct dentry *hfs_start_creating(struct dentry *dentry)
{
	struct dentry *parent = dget_parent(dentry);
	struct dentry *ret;

	ret = start_creating_dentry(hfs_lower_dentry(parent),
				    hfs_lower_dentry(dentry));
	dput(parent);
	return ret;
}

static struct dentry *hfs_start_removing(struct dentry *dentry)
{
	struct dentry *parent = dget_parent(dentry);
	struct dentry *ret;

	ret = start_removing_dentry(hfs_lower_dentry(parent),
				    hfs_lower_dentry(dentry));
	dput(parent);
	return ret;
}

/* After a create: the new name's dentry points at the lower one made. */
static void hfs_set_lower(struct dentry *dentry, struct dentry *lower)
{
	struct dentry *old = dentry->d_fsdata;

	/* Walks under RCU read it unlocked; the old one is freed after them. */
	if (old != lower) {
		WRITE_ONCE(dentry->d_fsdata, dget(lower));
		dput(old);
	}
}

static struct dentry *hfs_lookup(struct inode *dir, struct dentry *dentry,
				 unsigned int flags)
{
	struct dentry *lower_parent = hfs_lower_dentry(dentry->d_parent);
	struct dentry *lower;
	struct inode *inode, *lower_inode;
	struct qstr name = QSTR_INIT(dentry->d_name.name, dentry->d_name.len);

	lower = lookup_noperm_unlocked(&name, lower_parent);
	if (IS_ERR(lower))
		return ERR_CAST(lower);
	dentry->d_fsdata = lower;
	fsstack_copy_attr_atime(dir, d_inode(lower_parent));

	/* Read once: the parent is not locked, so it may become positive. */
	lower_inode = READ_ONCE(lower->d_inode);
	if (!lower_inode)
		return d_splice_alias(NULL, dentry);
	inode = __hfs_iget(dentry->d_sb, lower_inode);
	if (IS_ERR(inode))
		return ERR_CAST(inode);
	if (inode_state_read_once(inode) & I_NEW)
		unlock_new_inode(inode);
	return d_splice_alias(inode, dentry);
}

static int hfs_create(struct mnt_idmap *idmap, struct inode *dir,
		      struct dentry *dentry, umode_t mode, bool excl)
{
	struct dentry *lower;
	struct inode *lower_dir;
	int err;

	lower = hfs_start_creating(dentry);
	if (IS_ERR(lower))
		return PTR_ERR(lower);
	lower_dir = d_inode(lower->d_parent);
	err = vfs_create(&nop_mnt_idmap, lower, mode, NULL);
	if (!err) {
		hfs_set_lower(dentry, lower);
		err = hfs_interpose(lower, dentry);
		fsstack_copy_attr_times(dir, lower_dir);
		fsstack_copy_inode_size(dir, lower_dir);
	}
	end_creating(lower);
	return err;
}

static int hfs_mknod(struct mnt_idmap *idmap, struct inode *dir,
		     struct dentry *dentry, umode_t mode, dev_t dev)
{
	struct dentry *lower;
	struct inode *lower_dir;
	int err;

	lower = hfs_start_creating(dentry);
	if (IS_ERR(lower))
		return PTR_ERR(lower);
	lower_dir = d_inode(lower->d_parent);
	err = vfs_mknod(&nop_mnt_idmap, lower_dir, lower, mode, dev, NULL);
	if (!err && d_really_is_positive(lower)) {
		hfs_set_lower(dentry, lower);
		err = hfs_interpose(lower, dentry);
		fsstack_copy_attr_times(dir, lower_dir);
	}
	end_creating(lower);
	if (d_really_is_negative(dentry))
		d_drop(dentry);
	return err;
}

static int hfs_symlink(struct mnt_idmap *idmap, struct inode *dir,
		       struct dentry *dentry, const char *target)
{
	struct dentry *lower;
	struct inode *lower_dir;
	int err;

	lower = hfs_start_creating(dentry);
	if (IS_ERR(lower))
		return PTR_ERR(lower);
	lower_dir = d_inode(lower->d_parent);
	err = vfs_symlink(&nop_mnt_idmap, lower_dir, lower, target, NULL);
	if (!err && d_really_is_positive(lower)) {
		hfs_set_lower(dentry, lower);
		err = hfs_interpose(lower, dentry);
		fsstack_copy_attr_times(dir, lower_dir);
		fsstack_copy_inode_size(dir, lower_dir);
	}
	end_creating(lower);
	if (d_really_is_negative(dentry))
		d_drop(dentry);
	return err;
}

static struct dentry *hfs_mkdir(struct mnt_idmap *idmap, struct inode *dir,
				struct dentry *dentry, umode_t mode)
{
	struct dentry *lower, *parent;
	struct inode *lower_dir;
	int err;

	lower = hfs_start_creating(dentry);
	if (IS_ERR(lower))
		return lower;
	parent = dget(lower->d_parent);
	lower_dir = d_inode(parent);
	/* vfs_mkdir drops the dentry on failure and may replace it. */
	lower = vfs_mkdir(&nop_mnt_idmap, lower_dir, lower, mode, NULL);
	err = PTR_ERR_OR_ZERO(lower);
	if (!err && !d_unhashed(lower)) {
		hfs_set_lower(dentry, lower);
		err = hfs_interpose(lower, dentry);
		fsstack_copy_attr_times(dir, lower_dir);
		fsstack_copy_inode_size(dir, lower_dir);
		set_nlink(dir, lower_dir->i_nlink);
	}
	dput(parent);
	end_creating(lower);
	if (d_really_is_negative(dentry))
		d_drop(dentry);
	return ERR_PTR(err);
}

static int hfs_link(struct dentry *old, struct inode *dir, struct dentry *new)
{
	struct dentry *lower_old = hfs_lower_dentry(old);
	struct dentry *lower;
	struct inode *lower_dir;
	int err;

	lower = hfs_start_creating(new);
	if (IS_ERR(lower))
		return PTR_ERR(lower);
	lower_dir = d_inode(lower->d_parent);
	err = vfs_link(lower_old, &nop_mnt_idmap, lower_dir, lower, NULL);
	if (!err && d_really_is_positive(lower)) {
		hfs_set_lower(new, lower);
		err = hfs_interpose(lower, new);
		fsstack_copy_attr_times(dir, lower_dir);
		set_nlink(d_inode(old), hfs_lower_inode(d_inode(old))->i_nlink);
	}
	end_creating(lower);
	return err;
}

static int hfs_unlink(struct inode *dir, struct dentry *dentry)
{
	struct inode *inode = d_inode(dentry);
	struct dentry *lower;
	struct inode *lower_dir;
	int err;

	lower = hfs_start_removing(dentry);
	if (IS_ERR(lower))
		return PTR_ERR(lower);
	lower_dir = d_inode(lower->d_parent);
	err = vfs_unlink(&nop_mnt_idmap, lower_dir, lower, NULL);
	if (!err) {
		fsstack_copy_attr_times(dir, lower_dir);
		set_nlink(inode, hfs_lower_inode(inode)->i_nlink);
		inode_set_ctime_to_ts(inode, inode_get_ctime(dir));
	}
	end_removing(lower);
	if (!err)
		d_drop(dentry);
	return err;
}

static int hfs_rmdir(struct inode *dir, struct dentry *dentry)
{
	struct dentry *lower;
	struct inode *lower_dir;
	int err;

	lower = hfs_start_removing(dentry);
	if (IS_ERR(lower))
		return PTR_ERR(lower);
	lower_dir = d_inode(lower->d_parent);
	err = vfs_rmdir(&nop_mnt_idmap, lower_dir, lower, NULL);
	if (!err) {
		clear_nlink(d_inode(dentry));
		fsstack_copy_attr_times(dir, lower_dir);
		set_nlink(dir, lower_dir->i_nlink);
	}
	end_removing(lower);
	if (!err)
		d_drop(dentry);
	return err;
}

static int hfs_rename(struct mnt_idmap *idmap, struct inode *old_dir,
		      struct dentry *old, struct inode *new_dir,
		      struct dentry *new, unsigned int flags)
{
	struct inode *target = d_inode(new);
	struct renamedata rd = {};
	int err;

	rd.mnt_idmap = &nop_mnt_idmap;
	rd.old_parent = hfs_lower_dentry(old->d_parent);
	rd.new_parent = hfs_lower_dentry(new->d_parent);
	rd.flags = flags;
	err = start_renaming_two_dentries(&rd, hfs_lower_dentry(old),
					  hfs_lower_dentry(new));
	if (err)
		return err;
	err = vfs_rename(&rd);
	if (!err) {
		if (target)
			hfs_copy_attr(target, hfs_lower_inode(target));
		hfs_copy_attr(new_dir, d_inode(rd.new_parent));
		if (new_dir != old_dir)
			hfs_copy_attr(old_dir, d_inode(rd.old_parent));
	}
	end_renaming(&rd);
	return err;
}

static const char *hfs_get_link(struct dentry *dentry, struct inode *inode,
				struct delayed_call *done)
{
	if (!dentry)
		return ERR_PTR(-ECHILD);
	return vfs_get_link(hfs_lower_dentry(dentry), done);
}

/*
 * The lower filesystem's own permission check. The VFS has made the rest of
 * its checks on this inode already, the security module's among them, with
 * the lower inode's flags copied up (HFS_COPY_I_FLAGS); a path walk asks
 * this of every directory, so it is not done twice.
 */
static int hfs_permission(struct mnt_idmap *idmap, struct inode *inode,
			  int mask)
{
	struct inode *lower = hfs_lower_inode(inode);

	if (lower->i_op->permission)
		return lower->i_op->permission(&nop_mnt_idmap, lower, mask);
	return generic_permission(&nop_mnt_idmap, lower, mask);
}

static int hfs_setattr(struct mnt_idmap *idmap, struct dentry *dentry,
		       struct iattr *ia)
{
	struct inode *inode = d_inode(dentry);
	struct dentry *lower = hfs_lower_dentry(dentry);
	struct iattr lower_ia;
	int err;

	err = setattr_prepare(&nop_mnt_idmap, dentry, ia);
	if (err)
		return err;

	lower_ia = *ia;
	if (ia->ia_valid & ATTR_FILE)
		lower_ia.ia_file = hfs_lower_file(ia->ia_file);
	/* Clearing set-ID bits is the lower filesystem's to interpret. */
	if (lower_ia.ia_valid & (ATTR_KILL_SUID | ATTR_KILL_SGID))
		lower_ia.ia_valid &= ~ATTR_MODE;

	inode_lock(d_inode(lower));
	err = notify_change(&nop_mnt_idmap, lower, &lower_ia, NULL);
	inode_unlock(d_inode(lower));
	hfs_copy_attr(inode, hfs_lower_inode(inode));
	fsstack_copy_inode_size(inode, hfs_lower_inode(inode));
	return err;
}

static int hfs_getattr(struct mnt_idmap *idmap, const struct path *path,
		       struct kstat *stat, u32 request_mask,
		       unsigned int flags)
{
	struct dentry *dentry = path->dentry;
	struct inode *inode = d_inode(dentry);
	struct path lower;
	int err;

	hfs_lower_path(dentry, &lower);
	err = vfs_getattr_nosec(&lower, stat, request_mask, flags);
	if (err)
		return err;
	hfs_copy_attr(inode, hfs_lower_inode(inode));
	fsstack_copy_inode_size(inode, hfs_lower_inode(inode));
	/* The lower attributes, under this filesystem's device. */
	stat->dev = inode->i_sb->s_dev;
	return 0;
}

static ssize_t hfs_listxattr(struct dentry *dentry, char *list, size_t size)
{
	return vfs_listxattr(hfs_lower_dentry(dentry), list, size);
}

static int hfs_fileattr_get(struct dentry *dentry, struct file_kattr *fa)
{
	return vfs_fileattr_get(hfs_lower_dentry(dentry), fa);
}

static int hfs_fileattr_set(struct mnt_idmap *idmap, struct dentry *dentry,
			    struct file_kattr *fa)
{
	struct dentry *lower = hfs_lower_dentry(dentry);
	int err;

	err = vfs_fileattr_set(&nop_mnt_idmap, lower, fa);
	hfs_copy_attr(d_inode(dentry), d_inode(lower));
	return err;
}

static struct posix_acl *hfs_get_acl(struct mnt_idmap *idmap,
				     struct dentry *dentry, int type)
{
	return vfs_get_acl(&nop_mnt_idmap, hfs_lower_dentry(dentry),
			   posix_acl_xattr_name(type));
}

static int hfs_set_acl(struct mnt_idmap *idmap, struct dentry *dentry,
		       struct posix_acl *acl, int type)
{
	struct dentry *lower = hfs_lower_dentry(dentry);
	int err;

	err = vfs_set_acl(&nop_mnt_idmap, lower, posix_acl_xattr_name(type),
			  acl);
	if (!err)
		hfs_copy_attr(d_inode(dentry), d_inode(lower));
	return err;
}

const struct inode_operations hfs_dir_iops = {
	.lookup = hfs_lookup,
	.create = hfs_create,
	.mknod = hfs_mknod,
	.symlink = hfs_symlink,
	.mkdir = hfs_mkdir,
	.link = hfs_link,
	.unlink = hfs_unlink,
	.rmdir = hfs_rmdir,
	.rename = hfs_rename,
	.tmpfile = hfs_tmpfile,
	.permission = hfs_permission,
	.setattr = hfs_setattr,
	.getattr = hfs_getattr,
	.listxattr = hfs_listxattr,
	.fileattr_get = hfs_fileattr_get,
	.fileattr_set = hfs_fileattr_set,
	.get_acl = hfs_get_acl,
	.set_acl = hfs_set_acl,
};

const struct inode_operations hfs_file_iops = {
	.permission = hfs_permission,
	.setattr = hfs_setattr,
	.getattr = hfs_getattr,
	.listxattr = hfs_listxattr,
	.fileattr_get = hfs_fileattr_get,
	.fileattr_set = hfs_fileattr_set,
	.get_acl = hfs_get_acl,
	.set_acl = hfs_set_acl,
};

const struct inode_operations hfs_symlink_iops = {
	.get_link = hfs_get_link,
	.permission = hfs_permission,
	.setattr = hfs_setattr,
	.getattr = hfs_getattr,
	.listxattr = hfs_listxattr,
};

/* Every extended attribute, whatever its namespace, is the lower one's. */
static int hfs_xattr_get(const struct xattr_handler *handler,
			 struct dentry *dentry, struct inode *inode,
			 const char *name, void *buffer, size_t size)
{
	struct dentry *lower = hfs_lower_dentry(dentry);
	struct inode *lower_inode = hfs_lower_inode(inode);

	if (!(lower_inode->i_opflags & IOP_XATTR))
		return -EOPNOTSUPP;
	return __vfs_getxattr(lower, lower_inode, name, buffer, size);
}

static int hfs_xattr_set(const struct xattr_handler *handler,
			 struct mnt_idmap *idmap, struct dentry *dentry,
			 struct inode *inode, const char *name,
			 const void *value, size_t size, int flags)
{
	struct dentry *lower = hfs_lower_dentry(dentry);
	struct inode *lower_inode = hfs_lower_inode(inode);
	int err;

	if (!(lower_inode->i_opflags & IOP_XATTR))
		return -EOPNOTSUPP;
	inode_lock(lower_inode);
	if (value)
		err = __vfs_setxattr_locked(&nop_mnt_idmap, lower, name, value,
					    size, flags, NULL);
	else
		err = __vfs_removexattr_locked(&nop_mnt_idmap, lower, name,
					       NULL);
	inode_unlock(lower_inode);
	if (!err)
		hfs_copy_attr(inode, lower_inode);
	return err;
}

static const struct xattr_handler hfs_xattr_handler = {
	.prefix = "", /* every name */
	.get = hfs_xattr_get,
	.set = hfs_xattr_set,
};

const struct xattr_handler * const hfs_xattr_handlers[] = {
	&hfs_xattr_handler,
	NULL,
};
