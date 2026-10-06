// SPDX-License-Identifier: GPL-2.0
/*
 * hangarfs: mounting, the superblock, and dentries.
 *
 *	mount -t hangarfs [-o transparent] <lower directory> <mountpoint>
 *
 * With transparent, the mount reports the lower filesystem's stacking depth
 * rather than one more. The kernel bounds the depth to keep stacked calls
 * within a kernel stack; hangarfs's own calls are shallow (measured: an
 * overlayfs layer adds about 1.2 KiB on the deepest paths, of 16), and
 * without it an overlay mounted on the root -- rootless Podman's, a user's
 * own -- would be refused.
 */

#include "hangarfs.h"
#include <linux/fs_context.h>
#include <linux/fs_parser.h>
#include <linux/fs_stack.h>
#include <linux/init.h>
#include <linux/mount.h>
#include <linux/namei.h>
#include <linux/seq_file.h>
#include <linux/slab.h>
#include <linux/statfs.h>

static struct kmem_cache *hfs_inode_cache;

static struct inode *hfs_alloc_inode(struct super_block *sb)
{
	struct hfs_inode_info *hi;

	hi = alloc_inode_sb(sb, hfs_inode_cache, GFP_KERNEL);
	if (!hi)
		return NULL;
	hi->lower = NULL;
	return &hi->vfs_inode;
}

static void hfs_free_inode(struct inode *inode)
{
	kmem_cache_free(hfs_inode_cache, HFS_I(inode));
}

static void hfs_evict_inode(struct inode *inode)
{
	truncate_inode_pages_final(&inode->i_data);
	clear_inode(inode);
	iput(hfs_lower_inode(inode));
}

static int hfs_statfs(struct dentry *dentry, struct kstatfs *buf)
{
	struct path lower;
	int err;

	hfs_lower_path(dentry, &lower);
	err = vfs_statfs(&lower, buf);
	buf->f_type = HANGARFS_SUPER_MAGIC;
	return err;
}

static int hfs_show_options(struct seq_file *m, struct dentry *root)
{
	if (root->d_sb->s_stack_depth == HFS_SB(root->d_sb)->lower.dentry->d_sb->s_stack_depth)
		seq_puts(m, ",transparent");
	return 0;
}

static const struct super_operations hfs_sops = {
	.alloc_inode = hfs_alloc_inode,
	.free_inode = hfs_free_inode,
	.evict_inode = hfs_evict_inode,
	.statfs = hfs_statfs,
	.show_options = hfs_show_options,
};

/*
 * A cached name is still right if the lower one is, and the file is there.
 * The lower filesystem is reached only through this one, whose operations
 * keep the inodes' attributes in step, so a walk under RCU needs only the
 * lower dentry's own word: it stays a walk under RCU unless the lower
 * dentry was dropped or has a revalidation of its own.
 */
static int hfs_d_revalidate(struct inode *dir, const struct qstr *name,
			    struct dentry *dentry, unsigned int flags)
{
	struct dentry *lower = READ_ONCE(dentry->d_fsdata);
	int ret = 1;

	/* The lower filesystem dropped its dentry: look the name up again. */
	if (d_unhashed(lower))
		return flags & LOOKUP_RCU ? -ECHILD : 0;
	if (flags & LOOKUP_RCU) {
		struct inode *inode = d_inode_rcu(dentry);

		if (READ_ONCE(lower->d_flags) & DCACHE_OP_REVALIDATE)
			return -ECHILD;
		if (inode && !READ_ONCE(hfs_lower_inode(inode)->i_nlink))
			return -ECHILD;
		return 1;
	}
	if (lower->d_flags & DCACHE_OP_REVALIDATE) {
		struct name_snapshot n;

		take_dentry_name_snapshot(&n, lower);
		ret = lower->d_op->d_revalidate(hfs_lower_inode(dir), &n.name,
						lower, flags);
		release_dentry_name_snapshot(&n);
	}
	if (d_really_is_positive(dentry)) {
		struct inode *inode = d_inode(dentry);

		hfs_copy_attr(inode, hfs_lower_inode(inode));
		if (!inode->i_nlink)
			return 0;
	}
	return ret;
}

static void hfs_d_release(struct dentry *dentry)
{
	dput(dentry->d_fsdata);
}

const struct dentry_operations hfs_dops = {
	.d_revalidate = hfs_d_revalidate,
	.d_release = hfs_d_release,
};

enum { Opt_transparent };

static const struct fs_parameter_spec hfs_params[] = {
	fsparam_flag("transparent", Opt_transparent),
	{}
};

struct hfs_context {
	bool transparent;
};

static int hfs_parse_param(struct fs_context *fc, struct fs_parameter *param)
{
	struct hfs_context *ctx = fc->fs_private;
	struct fs_parse_result result;
	int opt;

	opt = fs_parse(fc, hfs_params, param, &result);
	if (opt < 0)
		return opt;
	if (opt == Opt_transparent)
		ctx->transparent = true;
	return 0;
}

static struct file_system_type hfs_type;

static int hfs_fill_super(struct super_block *sb, struct fs_context *fc)
{
	struct hfs_context *ctx = fc->fs_private;
	struct hfs_sb_info *sbi = sb->s_fs_info;
	struct super_block *lower_sb;
	struct inode *inode;
	int err;

	err = kern_path(fc->source, LOOKUP_FOLLOW | LOOKUP_DIRECTORY, &sbi->lower);
	if (err)
		return invalfc(fc, "no lower directory at %s", fc->source);
	lower_sb = sbi->lower.dentry->d_sb;
	if (lower_sb->s_type == &hfs_type)
		return invalfc(fc, "hangarfs over hangarfs");
	if (is_idmapped_mnt(sbi->lower.mnt))
		return invalfc(fc, "an idmapped lower mount");

	err = super_setup_bdi(sb);
	if (err)
		return err;
	sb->s_op = &hfs_sops;
	sb->s_xattr = hfs_xattr_handlers;
	set_default_d_op(sb, &hfs_dops);
	sb->s_magic = HANGARFS_SUPER_MAGIC;
	sb->s_maxbytes = lower_sb->s_maxbytes;
	sb->s_blocksize = lower_sb->s_blocksize;
	sb->s_blocksize_bits = lower_sb->s_blocksize_bits;
	sb->s_time_gran = lower_sb->s_time_gran;
	sb->s_flags |= lower_sb->s_flags & SB_POSIXACL;
	if (sb_rdonly(lower_sb))
		sb->s_flags |= SB_RDONLY;
	sb->s_stack_depth = lower_sb->s_stack_depth + (ctx->transparent ? 0 : 1);
	if (sb->s_stack_depth > FILESYSTEM_MAX_STACK_DEPTH)
		return invalfc(fc, "maximum filesystem stacking depth exceeded");

	inode = hfs_iget(sb, d_inode(sbi->lower.dentry));
	if (IS_ERR(inode))
		return PTR_ERR(inode);
	sb->s_root = d_make_root(inode);
	if (!sb->s_root)
		return -ENOMEM;
	sb->s_root->d_fsdata = dget(sbi->lower.dentry);
	return 0;
}

static int hfs_get_tree(struct fs_context *fc)
{
	if (!fc->source)
		return invalfc(fc, "no lower directory");
	return get_tree_nodev(fc, hfs_fill_super);
}

static void hfs_free_fc(struct fs_context *fc)
{
	kfree(fc->fs_private);
	kfree(fc->s_fs_info);
}

static const struct fs_context_operations hfs_context_ops = {
	.parse_param = hfs_parse_param,
	.get_tree = hfs_get_tree,
	.free = hfs_free_fc,
};

static int hfs_init_fs_context(struct fs_context *fc)
{
	fc->fs_private = kzalloc(sizeof(struct hfs_context), GFP_KERNEL);
	fc->s_fs_info = kzalloc(sizeof(struct hfs_sb_info), GFP_KERNEL);
	if (!fc->fs_private || !fc->s_fs_info)
		return -ENOMEM;
	fc->ops = &hfs_context_ops;
	return 0;
}

static void hfs_kill_sb(struct super_block *sb)
{
	struct hfs_sb_info *sbi = sb->s_fs_info;

	kill_anon_super(sb);
	if (sbi) {
		path_put(&sbi->lower);
		kfree(sbi);
	}
}

static struct file_system_type hfs_type = {
	.owner = THIS_MODULE,
	.name = "hangarfs",
	.init_fs_context = hfs_init_fs_context,
	.parameters = hfs_params,
	.kill_sb = hfs_kill_sb,
};

static void hfs_inode_init_once(void *p)
{
	struct hfs_inode_info *hi = p;

	inode_init_once(&hi->vfs_inode);
}

static int __init hfs_init(void)
{
	hfs_inode_cache = kmem_cache_create("hangarfs_inode",
					    sizeof(struct hfs_inode_info), 0,
					    SLAB_RECLAIM_ACCOUNT | SLAB_ACCOUNT,
					    hfs_inode_init_once);
	if (!hfs_inode_cache)
		return -ENOMEM;
	return register_filesystem(&hfs_type);
}
fs_initcall(hfs_init);
