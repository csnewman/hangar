// SPDX-License-Identifier: GPL-2.0
/*
 * hangarfs: open files. Each has the lower file open beside it, through
 * which its contents are read, written and mapped, and its directory read.
 * Locks are this filesystem's own: one on a file under a shared path is
 * first taken for the environment (sync.c), and every way a lock is let go
 * of -- unlocking, closing, the last reference going -- arrives here.
 */

#include "hangarfs.h"
#include <linux/backing-file.h>
#include <linux/fadvise.h>
#include <linux/file.h>
#include <linux/filelock.h>
#include <linux/fs_stack.h>
#include <linux/hangar_sync.h>
#include <linux/slab.h>
#include <linux/splice.h>

static int hfs_open(struct inode *inode, struct file *file)
{
	struct hfs_file *hf;
	struct file *lower;
	struct path path;

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
	file->private_data = hf;
	/* Direct I/O is the lower file's to do, where it can. */
	if (lower->f_mode & FMODE_CAN_ODIRECT)
		file->f_mode |= FMODE_CAN_ODIRECT;
	return 0;
}

static int hfs_release(struct inode *inode, struct file *file)
{
	struct hfs_file *hf = file->private_data;

	fput(hf->lower);
	kfree(hf);
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

/* The lower file's position is the one that counts: kept in step. */
static loff_t hfs_llseek(struct file *file, loff_t offset, int whence)
{
	struct file *lower = hfs_lower_file(file);
	loff_t ret;

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

	if (lower->f_op->flush)
		return lower->f_op->flush(lower, id);
	return 0;
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

	if (!lower->f_op->unlocked_ioctl)
		return -ENOTTY;
	return lower->f_op->unlocked_ioctl(lower, cmd, arg);
}

#ifdef CONFIG_COMPAT
static long hfs_compat_ioctl(struct file *file, unsigned int cmd,
			     unsigned long arg)
{
	struct file *lower = hfs_lower_file(file);

	if (!lower->f_op->compat_ioctl)
		return -ENOIOCTLCMD;
	return lower->f_op->compat_ioctl(lower, cmd, arg);
}
#endif

/*
 * POSIX and OFD locks. The lock is this inode's, kept by the VFS as for any
 * filesystem; taking one on a shared file is first the environment's.
 * Closing a file and its last reference going both unlock through here.
 */
static int hfs_lock(struct file *file, int cmd, struct file_lock *fl)
{
	int err;

	if (IS_GETLK(cmd)) {
		posix_test_lock(file, fl);
		return 0;
	}
	if (fl->c.flc_type != F_UNLCK) {
		err = hangar_sync_lock(file, fl, IS_SETLKW(cmd));
		if (err)
			return err;
	}
	err = posix_lock_file(file, fl, NULL);
	hangar_sync_changed(file);
	return err;
}

/* flock(2) locks, likewise; letting go of the last reference unlocks too. */
static int hfs_flock(struct file *file, int cmd, struct file_lock *fl)
{
	int err;

	if (fl->c.flc_type != F_UNLCK) {
		err = hangar_sync_lock(file, fl, fl->c.flc_flags & FL_SLEEP);
		if (err)
			return err;
	}
	err = locks_lock_file_wait(file, fl);
	hangar_sync_changed(file);
	return err;
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

/* A directory is read from the lower one, its position kept in step. */
static int hfs_iterate(struct file *file, struct dir_context *ctx)
{
	struct file *lower = hfs_lower_file(file);
	int err;

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
	.unlocked_ioctl = hfs_ioctl,
#ifdef CONFIG_COMPAT
	.compat_ioctl = hfs_compat_ioctl,
#endif
	.lock = hfs_lock,
	.flock = hfs_flock,
};
