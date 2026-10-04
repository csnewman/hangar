// SPDX-License-Identifier: GPL-2.0
/*
 * hangar-sync: locks on files an environment shares with others.
 *
 * Hangar's guest agent opens /dev/hangar-sync and registers the paths whose
 * files are shared. A process taking a lock on one of them, with flock(2) or
 * fcntl(2), first has the lock taken for the environment as a whole: the
 * kernel sends the agent a LOCK and waits for its reply, which the agent gets
 * from Hangar's server. Then the local lock is taken as usual, so processes
 * within the environment contend as they always do. When no process here
 * holds a lock on the file any more, the kernel sends IDLE and the agent
 * gives the environment's lock back.
 *
 * The environment's lock covers the whole file, whatever range was asked for:
 * coarser than POSIX allows, never unsafe.
 *
 * With no agent connected, or no paths registered, locks are local only, and
 * a lock on any other file costs nothing more than a check of a counter.
 */

#include <linux/dcache.h>
#include <linux/filelock.h>
#include <linux/fs.h>
#include <linux/hangar_sync.h>
#include <linux/init.h>
#include <linux/list.h>
#include <linux/miscdevice.h>
#include <linux/poll.h>
#include <linux/sched.h>
#include <linux/slab.h>
#include <linux/spinlock.h>
#include <linux/uaccess.h>
#include <linux/wait.h>
#include <uapi/linux/hangar_sync.h>

/* A registered path. */
struct hs_path {
	struct list_head list;
	size_t len;
	char path[];
};

/* A message waiting to be read by the agent. */
struct hs_msg {
	struct list_head list;
	struct hangar_sync_msg hdr;
	char path[];
};

/* A LOCK waiting for its reply. */
struct hs_wait {
	struct list_head list;
	u64 id;
	int result;
	bool done;
	wait_queue_head_t wq;
};

/* A file a LOCK was sent for, so its IDLE can be sent. */
struct hs_held {
	struct list_head list;
	struct inode *inode;
	/*
	 * LOCKs for it not yet answered. No IDLE is sent while there are: the
	 * agent writes the file's latest content as it takes the lock, and the
	 * close of that would otherwise find no lock here yet.
	 */
	unsigned int pending;
	size_t len;
	char path[];
};

static DEFINE_SPINLOCK(hs_lock);
static bool hs_connected;
static unsigned int hs_npaths;
static u64 hs_next_id;
static LIST_HEAD(hs_paths);
static LIST_HEAD(hs_out);
static LIST_HEAD(hs_waits);
static LIST_HEAD(hs_helds);
static DECLARE_WAIT_QUEUE_HEAD(hs_readq);

/* Whether path is a registered path or under one. Called with hs_lock. */
static bool hs_matches(const char *path, size_t len)
{
	struct hs_path *p;

	list_for_each_entry(p, &hs_paths, list) {
		if (len < p->len || memcmp(path, p->path, p->len))
			continue;
		if (len == p->len || path[p->len] == '/' ||
		    (p->len && p->path[p->len - 1] == '/'))
			return true;
	}
	return false;
}

static struct hs_msg *hs_msg_new(u32 op, const char *path, size_t len, gfp_t gfp)
{
	struct hs_msg *m = kzalloc(sizeof(*m) + len, gfp);

	if (!m)
		return NULL;
	m->hdr.len = sizeof(m->hdr) + len;
	m->hdr.op = op;
	m->hdr.path_len = len;
	memcpy(m->path, path, len);
	return m;
}

/* Queues a message for the agent. Called with hs_lock. */
static void hs_queue(struct hs_msg *m)
{
	list_add_tail(&m->list, &hs_out);
	wake_up_interruptible(&hs_readq);
}

static struct hs_held *hs_find_held(struct inode *inode)
{
	struct hs_held *h;

	list_for_each_entry(h, &hs_helds, list)
		if (h->inode == inode)
			return h;
	return NULL;
}

/**
 * hangar_sync_lock - take a lock on a shared file for the environment
 * @filp: the file being locked
 * @fl: the lock asked for
 * @wait: whether the caller waits for it
 *
 * Called before the local lock. Returns 0 when the environment holds the
 * file's lock (or the file is not shared), -EAGAIN when another environment
 * does and @wait is false, or -EINTR when a signal ended the wait.
 */
int hangar_sync_lock(struct file *filp, struct file_lock *fl, bool wait)
{
	struct hs_wait w = { .done = false };
	struct hs_held *held = NULL, *h;
	struct hs_msg *m;
	char *buf, *path;
	size_t len;
	int ret;

	if (fl->c.flc_type == F_UNLCK || !READ_ONCE(hs_npaths))
		return 0;

	buf = __getname();
	if (!buf)
		return 0;
	path = d_path(&filp->f_path, buf, PATH_MAX);
	if (IS_ERR(path)) {
		__putname(buf);
		return 0;
	}
	len = strlen(path);

	m = hs_msg_new(HANGAR_SYNC_LOCK, path, len, GFP_KERNEL);
	held = kzalloc(sizeof(*held) + len, GFP_KERNEL);
	if (!m || !held) {
		kfree(m);
		kfree(held);
		__putname(buf);
		return 0;
	}
	held->inode = file_inode(filp);
	held->len = len;
	memcpy(held->path, path, len);
	__putname(buf);

	m->hdr.type = fl->c.flc_type;
	m->hdr.flags = (wait ? HANGAR_SYNC_WAIT : 0) |
		       ((fl->c.flc_flags & FL_FLOCK) ? HANGAR_SYNC_FLOCK : 0);
	m->hdr.pid = task_tgid_vnr(current);
	m->hdr.start = fl->fl_start;
	m->hdr.end = fl->fl_end;
	init_waitqueue_head(&w.wq);

	spin_lock(&hs_lock);
	if (!hs_connected || !hs_matches(m->path, len)) {
		spin_unlock(&hs_lock);
		kfree(m);
		kfree(held);
		return 0;
	}
	w.id = m->hdr.id = ++hs_next_id;
	list_add_tail(&w.list, &hs_waits);
	h = hs_find_held(held->inode);
	if (!h) {
		list_add_tail(&held->list, &hs_helds);
		h = held;
		held = NULL;
	}
	h->pending++;
	hs_queue(m);
	spin_unlock(&hs_lock);
	kfree(held);

	ret = wait_event_interruptible(w.wq, READ_ONCE(w.done));

	spin_lock(&hs_lock);
	/* Found again: the agent's going away forgets every one. */
	h = hs_find_held(file_inode(filp));
	if (h && h->pending)
		h->pending--;
	if (!w.done) {
		/* Interrupted: the agent is told, and the lock not taken. */
		list_del(&w.list);
		if (hs_connected) {
			m = hs_msg_new(HANGAR_SYNC_CANCEL, NULL, 0, GFP_ATOMIC);
			if (m) {
				m->hdr.id = w.id;
				hs_queue(m);
			}
		}
		spin_unlock(&hs_lock);
		ret = ret ? -EINTR : 0;
	} else {
		spin_unlock(&hs_lock);
		ret = w.result;
	}
	/*
	 * Not taken here: if nothing else here holds a lock on the file, the
	 * agent gives back what it may have taken for it.
	 */
	if (ret)
		hangar_sync_changed(filp);
	return ret;
}

/**
 * hangar_sync_changed - a file's locks changed or were let go
 * @filp: the file
 *
 * Called after a lock or unlock, and when a close or the last reference to a
 * file lets go of its locks. If no process here holds a lock on a file a
 * LOCK was sent for, the agent is told it is idle.
 */
void hangar_sync_changed(struct file *filp)
{
	struct inode *inode = file_inode(filp);
	struct file_lock_context *ctx;
	struct hs_held *h;
	struct hs_msg *m;

	if (!READ_ONCE(hs_npaths) && list_empty_careful(&hs_helds))
		return;

	ctx = locks_inode_context(inode);
	if (ctx && (!list_empty_careful(&ctx->flc_flock) ||
		    !list_empty_careful(&ctx->flc_posix)))
		return;

	spin_lock(&hs_lock);
	h = hs_find_held(inode);
	if (!h || h->pending) {
		spin_unlock(&hs_lock);
		return;
	}
	list_del(&h->list);
	if (hs_connected) {
		m = hs_msg_new(HANGAR_SYNC_IDLE, h->path, h->len, GFP_ATOMIC);
		if (m)
			hs_queue(m);
	}
	spin_unlock(&hs_lock);
	kfree(h);
}

/* Lets every waiting LOCK go ahead and forgets everything: the agent is
 * gone, and locks are local only until it is back. Called with hs_lock. */
static void hs_reset(void)
{
	struct hs_path *p, *pn;
	struct hs_msg *m, *mn;
	struct hs_wait *w, *wn;
	struct hs_held *h, *hn;

	list_for_each_entry_safe(p, pn, &hs_paths, list) {
		list_del(&p->list);
		kfree(p);
	}
	WRITE_ONCE(hs_npaths, 0);
	list_for_each_entry_safe(m, mn, &hs_out, list) {
		list_del(&m->list);
		kfree(m);
	}
	list_for_each_entry_safe(w, wn, &hs_waits, list) {
		list_del_init(&w->list);
		w->result = 0;
		WRITE_ONCE(w->done, true);
		wake_up(&w->wq);
	}
	list_for_each_entry_safe(h, hn, &hs_helds, list) {
		list_del(&h->list);
		kfree(h);
	}
}

static int hs_open(struct inode *inode, struct file *file)
{
	if (!capable(CAP_SYS_ADMIN))
		return -EPERM;
	spin_lock(&hs_lock);
	if (hs_connected) {
		spin_unlock(&hs_lock);
		return -EBUSY;
	}
	hs_connected = true;
	spin_unlock(&hs_lock);
	return 0;
}

static int hs_release(struct inode *inode, struct file *file)
{
	spin_lock(&hs_lock);
	hs_connected = false;
	hs_reset();
	spin_unlock(&hs_lock);
	return 0;
}

static ssize_t hs_read(struct file *file, char __user *ubuf, size_t count,
		       loff_t *ppos)
{
	struct hs_msg *m;
	size_t len;
	int ret;

	for (;;) {
		spin_lock(&hs_lock);
		m = list_first_entry_or_null(&hs_out, struct hs_msg, list);
		if (m) {
			if (count < m->hdr.len) {
				spin_unlock(&hs_lock);
				return -EINVAL;
			}
			list_del(&m->list);
			spin_unlock(&hs_lock);
			break;
		}
		spin_unlock(&hs_lock);
		if (file->f_flags & O_NONBLOCK)
			return -EAGAIN;
		ret = wait_event_interruptible(hs_readq, !list_empty_careful(&hs_out));
		if (ret)
			return ret;
	}

	len = m->hdr.len;
	if (copy_to_user(ubuf, &m->hdr, sizeof(m->hdr)) ||
	    copy_to_user(ubuf + sizeof(m->hdr), m->path, m->hdr.path_len)) {
		kfree(m);
		return -EFAULT;
	}
	kfree(m);
	return len;
}

static ssize_t hs_write(struct file *file, const char __user *ubuf,
			size_t count, loff_t *ppos)
{
	struct hangar_sync_msg hdr;
	struct hs_path *p;
	struct hs_wait *w;

	if (count < sizeof(hdr) || copy_from_user(&hdr, ubuf, sizeof(hdr)))
		return -EINVAL;
	if (hdr.path_len > PATH_MAX || count < sizeof(hdr) + hdr.path_len)
		return -EINVAL;

	switch (hdr.op) {
	case HANGAR_SYNC_REPLY:
		spin_lock(&hs_lock);
		list_for_each_entry(w, &hs_waits, list) {
			if (w->id != hdr.id)
				continue;
			list_del_init(&w->list);
			w->result = hdr.result > 0 ? -EIO : hdr.result;
			WRITE_ONCE(w->done, true);
			wake_up(&w->wq);
			break;
		}
		spin_unlock(&hs_lock);
		return count;
	case HANGAR_SYNC_ADD_PATH:
		if (!hdr.path_len)
			return -EINVAL;
		p = kzalloc(sizeof(*p) + hdr.path_len, GFP_KERNEL);
		if (!p)
			return -ENOMEM;
		if (copy_from_user(p->path, ubuf + sizeof(hdr), hdr.path_len)) {
			kfree(p);
			return -EFAULT;
		}
		p->len = hdr.path_len;
		spin_lock(&hs_lock);
		list_add_tail(&p->list, &hs_paths);
		WRITE_ONCE(hs_npaths, hs_npaths + 1);
		spin_unlock(&hs_lock);
		return count;
	case HANGAR_SYNC_CLEAR_PATHS: {
		struct hs_path *pn;

		spin_lock(&hs_lock);
		list_for_each_entry_safe(p, pn, &hs_paths, list) {
			list_del(&p->list);
			kfree(p);
		}
		WRITE_ONCE(hs_npaths, 0);
		spin_unlock(&hs_lock);
		return count;
	}
	}
	return -EINVAL;
}

static __poll_t hs_poll(struct file *file, poll_table *wait)
{
	poll_wait(file, &hs_readq, wait);
	return list_empty_careful(&hs_out) ? EPOLLOUT : EPOLLIN | EPOLLOUT;
}

static const struct file_operations hs_fops = {
	.owner = THIS_MODULE,
	.open = hs_open,
	.release = hs_release,
	.read = hs_read,
	.write = hs_write,
	.poll = hs_poll,
	.llseek = noop_llseek,
};

static struct miscdevice hs_dev = {
	.minor = MISC_DYNAMIC_MINOR,
	.name = "hangar-sync",
	.fops = &hs_fops,
	.mode = 0600,
};

static int __init hangar_sync_init(void)
{
	return misc_register(&hs_dev);
}
device_initcall(hangar_sync_init);
