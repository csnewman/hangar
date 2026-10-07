// SPDX-License-Identifier: GPL-2.0
/*
 * hangarfs: open files. Each has the lower file open beside it, through
 * which its contents are read, written and mapped, and its directory read.
 * Locks on a routed file are the shared filesystem's, so they hold between
 * environments; on any other, this filesystem's own. A directory holding
 * route points is listed with them, from their targets.
 */

#include "hangarfs.h"
#include <linux/backing-file.h>
#include <linux/compat.h>
#include <linux/fadvise.h>
#include <linux/file.h>
#include <linux/filelock.h>
#include <linux/fs_stack.h>
#include <linux/fs_dirent.h>
#include <linux/slab.h>
#include <linux/splice.h>

struct hfs_dirent {
	u64 ino;
	unsigned int type;
	int len;
	char name[];
};

static void hfs_free_entries(struct hfs_file *hf)
{
	unsigned int i;

	for (i = 0; i < hf->nentries; i++)
		kfree(hf->entries[i]);
	kfree(hf->entries);
	hf->entries = NULL;
	hf->nentries = 0;
}

static void hfs_free_file(struct hfs_file *hf)
{
	hfs_free_entries(hf);
	kfree(hf->routed);
	kfree(hf);
}

static int hfs_open(struct inode *inode, struct file *file)
{
	struct hfs_file *hf;
	struct file *lower;
	struct path path;
	int n;

	hf = kzalloc(sizeof(*hf), GFP_KERNEL);
	if (!hf)
		return -ENOMEM;
	hfs_lower_path(file->f_path.dentry, &path);
	if (S_ISDIR(inode->i_mode))
		lower = kernel_file_open(&path, file->f_flags, current_cred());
	else
		lower = backing_file_open(file, file->f_flags, &path,
					  current_cred());
	if (IS_ERR(lower)) {
		kfree(hf);
		return PTR_ERR(lower);
	}
	hf->lower = lower;
	if (S_ISDIR(inode->i_mode)) {
		n = hfs_route_children(file->f_path.dentry, &hf->routed,
				       &hf->routed_len);
		if (n < 0) {
			fput(lower);
			kfree(hf);
			return n;
		}
	}
	file->private_data = hf;
	/* Direct I/O is the lower file's to do, where it can. */
	if (lower->f_mode & FMODE_CAN_ODIRECT)
		file->f_mode |= FMODE_CAN_ODIRECT;
	return 0;
}

static int hfs_tmpfile_open(struct inode *inode, struct file *file)
{
	return 0;
}

/*
 * O_TMPFILE: an unnamed file made in the lower directory, opened as the
 * lower file of this one, which is opened already when the VFS gets it.
 */
int hfs_tmpfile(struct mnt_idmap *idmap, struct inode *dir, struct file *file,
		umode_t mode)
{
	struct dentry *dentry = file->f_path.dentry;
	struct hfs_file *hf;
	struct file *lower;
	struct inode *inode;
	struct path parent;
	int err;

	hf = kzalloc(sizeof(*hf), GFP_KERNEL);
	if (!hf)
		return -ENOMEM;
	hfs_lower_path(dentry->d_parent, &parent);
	lower = backing_tmpfile_open(file, file->f_flags, &parent, mode,
				     current_cred());
	if (IS_ERR(lower)) {
		kfree(hf);
		return PTR_ERR(lower);
	}
	hf->lower = lower;
	inode = hfs_iget(dir->i_sb, file_inode(lower));
	if (IS_ERR(inode)) {
		err = PTR_ERR(inode);
		goto fail;
	}
	dentry->d_fsdata = dget(lower->f_path.dentry);
	d_instantiate(dentry, inode);
	file->private_data = hf;
	if (lower->f_mode & FMODE_CAN_ODIRECT)
		file->f_mode |= FMODE_CAN_ODIRECT;
	err = finish_open(file, dentry, hfs_tmpfile_open);
	if (file->f_mode & FMODE_OPENED)
		return err;
	file->private_data = NULL;
fail:
	fput(lower);
	kfree(hf);
	return err;
}

static int hfs_release(struct inode *inode, struct file *file)
{
	struct hfs_file *hf = file->private_data;

	fput(hf->lower);
	hfs_free_file(hf);
	return 0;
}

static void hfs_accessed(struct file *file)
{
	fsstack_copy_attr_atime(file_inode(file),
				file_inode(hfs_lower_file(file)));
}

static void hfs_end_write(struct kiocb *iocb, ssize_t ret)
{
	struct inode *inode = file_inode(iocb->ki_filp);
	struct inode *lower = hfs_lower_inode(inode);

	fsstack_copy_inode_size(inode, lower);
	fsstack_copy_attr_times(inode, lower);
}

static ssize_t hfs_read_iter(struct kiocb *iocb, struct iov_iter *iter)
{
	struct file *file = iocb->ki_filp;
	struct backing_file_ctx ctx = {
		.cred = file->f_cred,
		.accessed = hfs_accessed,
	};

	if (!iov_iter_count(iter))
		return 0;
	return backing_file_read_iter(hfs_lower_file(file), iter, iocb,
				      iocb->ki_flags, &ctx);
}

static ssize_t hfs_write_iter(struct kiocb *iocb, struct iov_iter *iter)
{
	struct file *file = iocb->ki_filp;
	struct backing_file_ctx ctx = {
		.cred = file->f_cred,
		.end_write = hfs_end_write,
	};

	if (!iov_iter_count(iter))
		return 0;
	return backing_file_write_iter(hfs_lower_file(file), iter, iocb,
				       iocb->ki_flags, &ctx);
}

static ssize_t hfs_splice_read(struct file *in, loff_t *ppos,
			       struct pipe_inode_info *pipe, size_t len,
			       unsigned int flags)
{
	struct kiocb iocb;
	struct backing_file_ctx ctx = {
		.cred = in->f_cred,
		.accessed = hfs_accessed,
	};
	ssize_t ret;

	init_sync_kiocb(&iocb, in);
	iocb.ki_pos = *ppos;
	ret = backing_file_splice_read(hfs_lower_file(in), &iocb, pipe, len,
				       flags, &ctx);
	*ppos = iocb.ki_pos;
	return ret;
}

static ssize_t hfs_splice_write(struct pipe_inode_info *pipe,
				struct file *out, loff_t *ppos, size_t len,
				unsigned int flags)
{
	struct kiocb iocb;
	struct backing_file_ctx ctx = {
		.cred = out->f_cred,
		.end_write = hfs_end_write,
	};
	ssize_t ret;

	init_sync_kiocb(&iocb, out);
	iocb.ki_pos = *ppos;
	ret = backing_file_splice_write(pipe, hfs_lower_file(out), &iocb, len,
					flags, &ctx);
	*ppos = iocb.ki_pos;
	return ret;
}

static int hfs_mmap(struct file *file, struct vm_area_struct *vma)
{
	struct backing_file_ctx ctx = {
		.cred = file->f_cred,
		.accessed = hfs_accessed,
	};

	return backing_file_mmap(hfs_lower_file(file), vma, &ctx);
}

/*
 * The lower file's position is the one that counts: kept in step. A
 * directory listed with its routed names has positions of its own.
 */
static loff_t hfs_llseek(struct file *file, loff_t offset, int whence)
{
	struct hfs_file *hf = file->private_data;
	struct file *lower = hfs_lower_file(file);
	loff_t ret;

	if (hf->routed) {
		if (whence == SEEK_CUR)
			offset += file->f_pos;
		else if (whence != SEEK_SET)
			return -EINVAL;
		if (offset < 0)
			return -EINVAL;
		file->f_pos = offset;
		return offset;
	}

	lower->f_pos = file->f_pos;
	ret = vfs_llseek(lower, offset, whence);
	file->f_pos = lower->f_pos;
	return ret;
}

static int hfs_fsync(struct file *file, loff_t start, loff_t end,
		     int datasync)
{
	return vfs_fsync_range(hfs_lower_file(file), start, end, datasync);
}

static int hfs_flush(struct file *file, fl_owner_t id)
{
	struct file *lower = hfs_lower_file(file);
	int err = 0;

	if (lower->f_op->flush)
		err = lower->f_op->flush(lower, id);
	/*
	 * Closing any descriptor of a file lets go of the process's locks on
	 * it: those on a routed file are the lower one's (hfs_lock).
	 */
	if (lower->f_op->lock)
		locks_remove_posix(lower, id);
	return err;
}

static long hfs_fallocate(struct file *file, int mode, loff_t offset,
			  loff_t len)
{
	struct inode *inode = file_inode(file);
	long ret;

	ret = vfs_fallocate(hfs_lower_file(file), mode, offset, len);
	fsstack_copy_inode_size(inode, hfs_lower_inode(inode));
	fsstack_copy_attr_times(inode, hfs_lower_inode(inode));
	return ret;
}

static int hfs_fadvise(struct file *file, loff_t offset, loff_t len,
		       int advice)
{
	return vfs_fadvise(hfs_lower_file(file), offset, len, advice);
}

static long hfs_ioctl(struct file *file, unsigned int cmd, unsigned long arg)
{
	struct file *lower = hfs_lower_file(file);

	if (cmd == HANGARFS_IOC_ROUTES)
		return hfs_set_routes(file, (void __user *)arg);
	if (!lower->f_op->unlocked_ioctl)
		return -ENOTTY;
	return lower->f_op->unlocked_ioctl(lower, cmd, arg);
}

#ifdef CONFIG_COMPAT
static long hfs_compat_ioctl(struct file *file, unsigned int cmd,
			     unsigned long arg)
{
	struct file *lower = hfs_lower_file(file);

	if (cmd == HANGARFS_IOC_ROUTES)
		return hfs_set_routes(file, compat_ptr(arg));
	if (!lower->f_op->compat_ioctl)
		return -ENOIOCTLCMD;
	return lower->f_op->compat_ioctl(lower, cmd, arg);
}
#endif

/*
 * POSIX and OFD locks. On a routed file the lock is the shared filesystem's,
 * taken there, which makes it every environment's: the request names the
 * lower file while it is passed down, since NFS keeps it to take the lock
 * again after a server restart. On any other file the lock is this inode's,
 * kept by the VFS as for any filesystem.
 */
static int hfs_lock(struct file *file, int cmd, struct file_lock *fl)
{
	struct file *lower = hfs_lower_file(file);
	int err;

	if (lower->f_op->lock) {
		fl->c.flc_file = lower;
		err = lower->f_op->lock(lower, cmd, fl);
		fl->c.flc_file = file;
		return err;
	}
	if (IS_GETLK(cmd)) {
		posix_test_lock(file, fl);
		return 0;
	}
	return posix_lock_file(file, fl, NULL);
}

/*
 * flock(2) locks, likewise. A routed file's is let go of with the lower
 * file, when this one's last reference goes.
 */
static int hfs_flock(struct file *file, int cmd, struct file_lock *fl)
{
	struct file *lower = hfs_lower_file(file);
	int err;

	if (lower->f_op->flock) {
		fl->c.flc_file = lower;
		err = lower->f_op->flock(lower, cmd, fl);
		fl->c.flc_file = file;
		return err;
	}
	return locks_lock_file_wait(file, fl);
}

const struct file_operations hfs_file_fops = {
	.open = hfs_open,
	.release = hfs_release,
	.read_iter = hfs_read_iter,
	.write_iter = hfs_write_iter,
	.splice_read = hfs_splice_read,
	.splice_write = hfs_splice_write,
	.mmap = hfs_mmap,
	.llseek = hfs_llseek,
	.fsync = hfs_fsync,
	.flush = hfs_flush,
	.fallocate = hfs_fallocate,
	.fadvise = hfs_fadvise,
	.unlocked_ioctl = hfs_ioctl,
#ifdef CONFIG_COMPAT
	.compat_ioctl = hfs_compat_ioctl,
#endif
	.lock = hfs_lock,
	.flock = hfs_flock,
};

struct hfs_merge {
	struct dir_context ctx;
	struct hfs_file *hf;
	unsigned int cap;
	int err;
};

static int hfs_add_entry(struct hfs_merge *m, const char *name, int len,
			 u64 ino, unsigned int type)
{
	struct hfs_file *hf = m->hf;
	struct hfs_dirent *e;

	if (hf->nentries == m->cap) {
		unsigned int cap = m->cap ? m->cap * 2 : 64;
		struct hfs_dirent **entries;

		entries = krealloc_array(hf->entries, cap, sizeof(*entries),
					 GFP_KERNEL);
		if (!entries)
			return -ENOMEM;
		hf->entries = entries;
		m->cap = cap;
	}
	e = kmalloc(struct_size(e, name, len), GFP_KERNEL);
	if (!e)
		return -ENOMEM;
	e->ino = ino;
	e->type = type;
	e->len = len;
	memcpy(e->name, name, len);
	hf->entries[hf->nentries++] = e;
	return 0;
}

/* Whether a name in the directory is a route point, served elsewhere. */
static bool hfs_is_routed_name(const struct hfs_file *hf, const char *name,
			       int len)
{
	const char *p = hf->routed, *end = hf->routed + hf->routed_len;

	while (p < end) {
		size_t n = strlen(p);

		if (n == len && !memcmp(p, name, len))
			return true;
		p += n + 1;
		p += strlen(p) + 1;
	}
	return false;
}

static bool hfs_merge_actor(struct dir_context *ctx, const char *name,
			    int len, loff_t offset, u64 ino, unsigned int type)
{
	struct hfs_merge *m = container_of(ctx, struct hfs_merge, ctx);

	if (hfs_is_routed_name(m->hf, name, len))
		return true;
	m->err = hfs_add_entry(m, name, len, ino, type);
	return !m->err;
}

/*
 * Lists a directory holding route points whole: the lower directory's
 * names but those, then each route point whose target is there, as the
 * target is.
 */
static int hfs_list_merged(struct file *file)
{
	struct hfs_file *hf = file->private_data;
	struct file *lower = hf->lower;
	struct hfs_merge m = { .ctx.actor = hfs_merge_actor, .hf = hf };
	const char *p, *end = hf->routed + hf->routed_len;
	unsigned int before;
	char *target;
	int err;

	hfs_free_entries(hf);
	lower->f_pos = 0;
	do {
		before = hf->nentries;
		m.ctx.pos = lower->f_pos;
		err = iterate_dir(lower, &m.ctx);
		if (!err)
			err = m.err;
	} while (!err && hf->nentries != before);
	if (err)
		return err;

	target = __getname();
	if (!target)
		return -ENOMEM;
	for (p = hf->routed; p < end && !err; ) {
		const char *name = p, *t = p + strlen(p) + 1;
		struct dentry *d;

		p = t + strlen(t) + 1;
		if (!*t) {
			/* Excluded: the lower filesystem's own, if it is there. */
			d = hfs_local_lookup(file->f_path.dentry, name, false);
		} else {
			strscpy(target, t, PATH_MAX);
			d = hfs_route_lookup(file_inode(file)->i_sb, target);
		}
		if (IS_ERR(d))
			continue;
		if (d_really_is_positive(d))
			err = hfs_add_entry(&m, name, strlen(name),
					    d_inode(d)->i_ino,
					    fs_umode_to_dtype(d_inode(d)->i_mode));
		dput(d);
	}
	__putname(target);
	return err;
}

/* A directory is read from the lower one, its position kept in step. */
static int hfs_iterate(struct file *file, struct dir_context *ctx)
{
	struct hfs_file *hf = file->private_data;
	struct file *lower = hfs_lower_file(file);
	int err;

	if (hf->routed) {
		if (ctx->pos == 0 || !hf->entries) {
			err = hfs_list_merged(file);
			if (err)
				return err;
		}
		while (ctx->pos < hf->nentries) {
			struct hfs_dirent *e = hf->entries[ctx->pos];

			if (!dir_emit(ctx, e->name, e->len, e->ino, e->type))
				break;
			ctx->pos++;
		}
		return 0;
	}

	lower->f_pos = ctx->pos;
	err = iterate_dir(lower, ctx);
	file->f_pos = lower->f_pos;
	fsstack_copy_attr_atime(file_inode(file), file_inode(lower));
	return err;
}

const struct file_operations hfs_dir_fops = {
	.open = hfs_open,
	.release = hfs_release,
	.iterate_shared = hfs_iterate,
	.read = generic_read_dir,
	.llseek = hfs_llseek,
	.fsync = hfs_fsync,
	.flush = hfs_flush,
	.unlocked_ioctl = hfs_ioctl,
#ifdef CONFIG_COMPAT
	.compat_ioctl = hfs_compat_ioctl,
#endif
	.lock = hfs_lock,
	.flock = hfs_flock,
};
