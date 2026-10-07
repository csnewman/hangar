// SPDX-License-Identifier: GPL-2.0
/*
 * hangarfs routes: names served from the shared directory rather than the
 * filesystem under the mount (include/uapi/linux/hangarfs.h).
 *
 * A routed name's lower dentry is the target's, in the shared directory's
 * filesystem, and everything below it follows from there: a routed
 * directory's children are looked up in the target, a routed file is read,
 * written and locked there. Only the route points themselves, and a routed
 * file's lock file (<name>.lock, which git and others make exclusively to
 * lock the file), are matched by path, at lookup.
 *
 * The routes are replaced whole, under RCU. A cached name records the
 * generation of the routes it was looked up under (d_time), and is checked
 * again when that changes: when the routes are replaced, or a directory
 * holding a route point, or within a routed one, is renamed.
 */

#include "hangarfs.h"
#include <linux/file.h>
#include <linux/namei.h>
#include <linux/slab.h>
#include <linux/uaccess.h>

#define HFS_LOCK_SUFFIX		".lock"
#define HFS_LOCK_SUFFIX_LEN	(sizeof(HFS_LOCK_SUFFIX) - 1)

static bool hfs_route_path_valid(const char *p, size_t len)
{
	const char *c;

	if (len < 2 || p[0] != '/' || p[len - 1] == '/')
		return false;
	for (c = p; c < p + len; c++) {
		if (*c != '/')
			continue;
		/* An empty, "." or ".." component. */
		if (c[1] == '/' || c[1] == '\0' ||
		    (c[1] == '.' && (c[2] == '/' || c[2] == '\0')) ||
		    (c[1] == '.' && c[2] == '.' && (c[3] == '/' || c[3] == '\0')))
			return false;
	}
	return true;
}

static bool hfs_route_target_valid(const char *t, size_t len)
{
	const char *c = t;

	if (!len || t[0] == '/' || t[len - 1] == '/')
		return false;
	while (c < t + len) {
		const char *end = strchrnul(c, '/');

		if (end == c || (end - c == 1 && c[0] == '.') ||
		    (end - c == 2 && c[0] == '.' && c[1] == '.'))
			return false;
		c = *end ? end + 1 : end;
	}
	return true;
}

/*
 * Reads the records into a table holding its own copy of the strings,
 * which the routes point into.
 */
static struct hfs_routes *hfs_routes_parse(const char *buf, size_t len)
{
	struct hfs_routes *routes;
	const char *p = buf, *end = buf + len;
	unsigned int n = 0, i;
	char *strings;

	/* Count, checking each record is whole. */
	while (p < end) {
		const char *path, *target;

		if (*p != HANGARFS_ROUTE_FILE && *p != HANGARFS_ROUTE_DIR &&
		    *p != HANGARFS_ROUTE_LOCK)
			return ERR_PTR(-EINVAL);
		path = p + 1;
		target = memchr(path, '\0', end - path);
		if (!target)
			return ERR_PTR(-EINVAL);
		target++;
		p = memchr(target, '\0', end - target);
		if (!p)
			return ERR_PTR(-EINVAL);
		if (!hfs_route_path_valid(path, target - 1 - path) ||
		    !hfs_route_target_valid(target, p - target))
			return ERR_PTR(-EINVAL);
		p++;
		if (++n > HANGARFS_ROUTES_MAX)
			return ERR_PTR(-E2BIG);
	}

	routes = kzalloc(struct_size(routes, r, n) + len, GFP_KERNEL);
	if (!routes)
		return ERR_PTR(-ENOMEM);
	strings = (char *)&routes->r[n];
	memcpy(strings, buf, len);
	routes->n = n;
	for (i = 0, p = strings; i < n; i++) {
		struct hfs_route *r = &routes->r[i];

		r->dir = *p == HANGARFS_ROUTE_DIR;
		r->lock = *p == HANGARFS_ROUTE_LOCK;
		r->path = p + 1;
		r->len = strlen(r->path);
		r->target = r->path + r->len + 1;
		p = r->target + strlen(r->target) + 1;
	}
	return routes;
}

long hfs_set_routes(struct file *file, struct hangarfs_routes __user *uarg)
{
	struct hfs_sb_info *sbi = HFS_SB(file_inode(file)->i_sb);
	struct hangarfs_routes arg;
	struct hfs_routes *routes = NULL, *old;
	char *buf;
	long err = 0;

	if (!capable(CAP_SYS_ADMIN))
		return -EPERM;
	if (copy_from_user(&arg, uarg, sizeof(arg)))
		return -EFAULT;
	if (arg.len > HANGARFS_ROUTES_MAX_LEN)
		return -E2BIG;
	if (arg.len) {
		buf = memdup_user(u64_to_user_ptr(arg.buf), arg.len);
		if (IS_ERR(buf))
			return PTR_ERR(buf);
		routes = hfs_routes_parse(buf, arg.len);
		kfree(buf);
		if (IS_ERR(routes))
			return PTR_ERR(routes);
	}

	mutex_lock(&sbi->routes_mutex);
	if (routes) {
		CLASS(fd, f)(arg.shared_fd);

		if (fd_empty(f)) {
			err = -EBADF;
			goto out;
		}
		if (!d_can_lookup(fd_file(f)->f_path.dentry) ||
		    fd_file(f)->f_path.dentry->d_sb == file_inode(file)->i_sb) {
			err = -EINVAL;
			goto out;
		}
		/* Routed dentries hold the shared filesystem's: it stays. */
		if (!sbi->shared.dentry) {
			sbi->shared = fd_file(f)->f_path;
			path_get(&sbi->shared);
		} else if (!path_equal(&sbi->shared, &fd_file(f)->f_path)) {
			err = -EBUSY;
			goto out;
		}
	}
	old = rcu_replace_pointer(sbi->routes, routes,
				  lockdep_is_held(&sbi->routes_mutex));
	routes = NULL;
	atomic_long_inc(&sbi->routes_gen);
	if (old)
		kfree_rcu(old, rcu);
out:
	mutex_unlock(&sbi->routes_mutex);
	kfree(routes);
	return err;
}

void hfs_free_routes(struct hfs_sb_info *sbi)
{
	kfree(rcu_dereference_protected(sbi->routes, 1));
	if (sbi->shared.dentry)
		path_put(&sbi->shared);
}

/* A route p is at or under, with its length. */
static enum hfs_route_kind hfs_route_match(const struct hfs_route *r,
					   const char *p, size_t len,
					   char *target)
{
	if (len < r->len || memcmp(p, r->path, r->len))
		return HFS_LOCAL;
	if (len == r->len) {
		if (target && strscpy(target, r->target, PATH_MAX) < 0)
			return HFS_LOCAL;
		return r->lock ? HFS_LOCK : HFS_ROUTED;
	}
	if (r->dir)
		return p[r->len] == '/' ? HFS_UNDER : HFS_LOCAL;
	/* A file's lock file, beside it, so making it is exclusive everywhere. */
	if (r->lock || len != r->len + HFS_LOCK_SUFFIX_LEN ||
	    memcmp(p + r->len, HFS_LOCK_SUFFIX, HFS_LOCK_SUFFIX_LEN))
		return HFS_LOCAL;
	if (target) {
		size_t tlen = strlen(r->target);

		if (tlen + HFS_LOCK_SUFFIX_LEN >= PATH_MAX)
			return HFS_LOCAL;
		memcpy(target, r->target, tlen);
		memcpy(target + tlen, HFS_LOCK_SUFFIX, HFS_LOCK_SUFFIX_LEN + 1);
	}
	return HFS_LOCK;
}

/*
 * Where a name is served from: here, a route's target (written to target,
 * PATH_MAX bytes, when given), or under a routed directory. A route point
 * itself is matched before another's lock file.
 */
int hfs_route(const struct dentry *dentry, char *target)
{
	struct hfs_sb_info *sbi = HFS_SB(dentry->d_sb);
	const struct hfs_routes *routes;
	int kind = HFS_LOCAL;
	unsigned int i;
	size_t len;
	char *buf, *p;

	if (!rcu_access_pointer(sbi->routes))
		return HFS_LOCAL;
	buf = __getname();
	if (!buf)
		return -ENOMEM;
	p = dentry_path_raw(dentry, buf, PATH_MAX);
	if (IS_ERR(p)) {
		kind = PTR_ERR(p);
		goto out;
	}
	len = strlen(p);

	rcu_read_lock();
	routes = rcu_dereference(sbi->routes);
	for (i = 0; routes && i < routes->n; i++) {
		const struct hfs_route *r = &routes->r[i];

		if (len == r->len && !memcmp(p, r->path, len)) {
			kind = hfs_route_match(r, p, len, target);
			goto unlock;
		}
	}
	for (i = 0; routes && i < routes->n; i++) {
		kind = hfs_route_match(&routes->r[i], p, len, target);
		if (kind != HFS_LOCAL)
			break;
	}
unlock:
	rcu_read_unlock();
out:
	__putname(buf);
	return kind;
}

/*
 * Whether renaming a directory could change how names under it are routed:
 * it holds a route point, or is in a routed directory or one itself.
 */
bool hfs_routes_cover(const struct dentry *dentry)
{
	struct hfs_sb_info *sbi = HFS_SB(dentry->d_sb);
	const struct hfs_routes *routes;
	bool cover = false;
	unsigned int i;
	size_t len;
	char *buf, *p;

	if (!rcu_access_pointer(sbi->routes))
		return false;
	buf = __getname();
	if (!buf)
		return true;
	p = dentry_path_raw(dentry, buf, PATH_MAX);
	if (IS_ERR(p)) {
		__putname(buf);
		return true;
	}
	len = strlen(p);

	rcu_read_lock();
	routes = rcu_dereference(sbi->routes);
	for (i = 0; routes && i < routes->n && !cover; i++) {
		const struct hfs_route *r = &routes->r[i];

		if (r->len > len && !memcmp(r->path, p, len) && r->path[len] == '/')
			cover = true;
		else if (hfs_route_match(r, p, len, NULL) != HFS_LOCAL)
			cover = true;
	}
	rcu_read_unlock();
	__putname(buf);
	return cover;
}

/*
 * A target's dentry in the shared directory: positive if it is there,
 * negative in its parent if not, which must be.
 */
struct dentry *hfs_route_lookup(struct super_block *sb, char *target)
{
	struct hfs_sb_info *sbi = HFS_SB(sb);
	char *slash = strrchr(target, '/');
	const char *name = target;
	struct path dir;
	struct dentry *lower;
	int err;

	if (slash) {
		*slash = '\0';
		err = vfs_path_lookup(sbi->shared.dentry, sbi->shared.mnt, target,
				      LOOKUP_DIRECTORY, &dir);
		*slash = '/';
		if (err)
			return ERR_PTR(err);
		name = slash + 1;
	} else {
		dir = sbi->shared;
		path_get(&dir);
	}
	lower = lookup_noperm_unlocked(&QSTR(name), dir.dentry);
	path_put(&dir);
	return lower;
}

/*
 * The route points directly in a directory, for listing it: their names,
 * and their targets after them, each ending in a NUL, in one allocation.
 */
int hfs_route_children(const struct dentry *dir, char **out, size_t *out_len)
{
	struct hfs_sb_info *sbi = HFS_SB(dir->d_sb);
	const struct hfs_routes *routes;
	size_t len, need = 0;
	unsigned int i, n = 0;
	char *buf, *p, *list = NULL, *w;

	*out = NULL;
	*out_len = 0;
	if (!rcu_access_pointer(sbi->routes))
		return 0;
	buf = __getname();
	if (!buf)
		return -ENOMEM;
	p = dentry_path_raw(dir, buf, PATH_MAX);
	if (IS_ERR(p)) {
		__putname(buf);
		return PTR_ERR(p);
	}
	len = strlen(p);
	/* The root's children are "/name". */
	if (len == 1)
		len = 0;

again:
	rcu_read_lock();
	routes = rcu_dereference(sbi->routes);
	for (i = 0, n = 0, w = list; routes && i < routes->n; i++) {
		const struct hfs_route *r = &routes->r[i];
		const char *name = r->path + len + 1;
		size_t nlen, tlen;

		if (r->len <= len + 1 || memcmp(r->path, p, len) ||
		    r->path[len] != '/' || strchr(name, '/'))
			continue;
		nlen = strlen(name) + 1;
		tlen = strlen(r->target) + 1;
		if (!list) {
			need += nlen + tlen;
		} else if (w + nlen + tlen <= list + need) {
			memcpy(w, name, nlen);
			memcpy(w + nlen, r->target, tlen);
			w += nlen + tlen;
		}
		n++;
	}
	rcu_read_unlock();
	if (n && !list) {
		list = kmalloc(need, GFP_KERNEL);
		if (!list) {
			__putname(buf);
			return -ENOMEM;
		}
		goto again;
	}
	__putname(buf);
	if (!n) {
		kfree(list);
		return 0;
	}
	*out = list;
	*out_len = w - list;
	return n;
}
