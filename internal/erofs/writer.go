package erofs

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path"
	"sort"
	"strings"
	"time"
)

// Entry is one thing in an image's tree.
type Entry struct {
	// Path is where it is, relative to the root, which is ".".
	Path string
	Type Type
	// Perm is its permission bits, with setuid, setgid and sticky (07777).
	Perm     uint32
	UID, GID uint32
	ModTime  time.Time
	// Size is a regular file's length: what Add reads of its data.
	Size int64
	// Target is where a symlink points.
	Target string
	// Major and Minor are a device's number.
	Major, Minor uint32
	// Xattrs are its extended attributes, by their full names. Those under
	// a prefix EROFS has no index for (anything in system. but the ACLs)
	// are left out.
	Xattrs map[string][]byte
}

// Writer builds an image. Entries come in any order: a file may come
// before its directory, which is made for it, owned by root with mode
// 0755, until the directory's own entry says otherwise.
type Writer struct {
	out  io.WriterAt
	root *node
	// next is the next free block: file data is written from block 1, after
	// the superblock's, as it arrives.
	next  int64
	buf   []byte
	tails []byte
	size  int64
	done  bool
}

type node struct {
	mode     uint16
	uid, gid uint32
	mtime    int64
	nsec     uint32
	size     int64
	rdev     uint32
	xattrs   []byte
	icount   uint16

	layout uint8
	start  uint32
	// tailOff and tailLen are the data kept with the inode: the part of a
	// file or symlink after its last whole block, in Writer.tails.
	tailOff, tailLen int

	children map[string]*node
	parent   *node
	implicit bool
	links    uint32

	queued bool
	nid    uint64
	ino    uint32

	// A directory's entries, sorted, and where each of its blocks starts in
	// them.
	dirents []dirent
	dirCuts []int
	dirTail int64
}

type dirent struct {
	name string
	n    *node
}

func (n *node) typ() Type { return typeOfMode(n.mode) }

// NewWriter starts an image in out, which it writes from offset 0: each
// file's data as Add is given it, and the rest at Close.
func NewWriter(out io.WriterAt) *Writer {
	root := &node{mode: typeModes[TypeDir] | 0o755, children: map[string]*node{}, implicit: true}
	root.parent = root
	return &Writer{out: out, root: root, next: 1, buf: make([]byte, 1<<20)}
}

// cleanPath is a path as the tree keys it: relative, without "./", "." for
// the root. A path cannot leave the root.
func cleanPath(p string) (string, error) {
	c := path.Clean("/" + p)
	if c == "/" {
		return ".", nil
	}
	c = c[1:]
	for _, part := range strings.Split(c, "/") {
		if len(part) > maxName {
			return "", fmt.Errorf("%s: a name longer than %d bytes", p, maxName)
		}
	}
	return c, nil
}

func (w *Writer) lookup(p string) *node {
	n := w.root
	if p == "." {
		return n
	}
	for part := range strings.SplitSeq(p, "/") {
		if n.children == nil {
			return nil
		}
		if n = n.children[part]; n == nil {
			return nil
		}
	}
	return n
}

// parentOf is the directory p goes in, made if it is not there yet.
func (w *Writer) parentOf(p string) (*node, error) {
	dir := path.Dir(p)
	n := w.root
	if dir == "." {
		return n, nil
	}
	for part := range strings.SplitSeq(dir, "/") {
		c := n.children[part]
		if c == nil {
			c = &node{mode: typeModes[TypeDir] | 0o755, children: map[string]*node{}, implicit: true, parent: n}
			n.children[part] = c
		} else if c.children == nil {
			return nil, fmt.Errorf("%s: %s is not a directory", p, part)
		}
		n = c
	}
	return n, nil
}

// Add puts e in the image, reading a regular file's e.Size bytes from data.
// A directory added where one was made for what it holds keeps what it
// holds and takes e's owner, mode and attributes; anything else may be
// added once.
func (w *Writer) Add(e Entry, data io.Reader) error {
	if w.done {
		return errors.New("erofs: the image is closed")
	}
	p, err := cleanPath(e.Path)
	if err != nil {
		return err
	}
	mode, ok := typeModes[e.Type]
	if !ok {
		return fmt.Errorf("%s: unknown type %d", e.Path, e.Type)
	}
	xattrs, icount, err := encodeXattrs(e.Xattrs)
	if err != nil {
		return fmt.Errorf("%s: %w", e.Path, err)
	}
	n := &node{
		mode: mode | uint16(e.Perm&0o7777), uid: e.UID, gid: e.GID,
		xattrs: xattrs, icount: icount, start: nullAddr,
	}
	if !e.ModTime.IsZero() {
		n.mtime, n.nsec = e.ModTime.Unix(), uint32(e.ModTime.Nanosecond())
	}

	existing := w.lookup(p)
	if e.Type == TypeDir {
		if existing != nil {
			if existing.children == nil {
				return fmt.Errorf("%s: added twice", e.Path)
			}
			if !existing.implicit {
				return fmt.Errorf("%s: added twice", e.Path)
			}
			existing.mode, existing.uid, existing.gid = n.mode, n.uid, n.gid
			existing.mtime, existing.nsec = n.mtime, n.nsec
			existing.xattrs, existing.icount = n.xattrs, n.icount
			existing.implicit = false
			return nil
		}
		n.children = map[string]*node{}
	} else if existing != nil {
		return fmt.Errorf("%s: added twice", e.Path)
	}
	if p == "." {
		return fmt.Errorf("the root is a directory, not a %s", e.Type)
	}
	parent, err := w.parentOf(p)
	if err != nil {
		return err
	}

	switch e.Type {
	case TypeRegular:
		if e.Size < 0 {
			return fmt.Errorf("%s: a negative size", e.Path)
		}
		if err := w.writeData(n, data, e.Size); err != nil {
			return fmt.Errorf("%s: %w", e.Path, err)
		}
	case TypeSymlink:
		if e.Target == "" {
			return fmt.Errorf("%s: a symlink to nothing", e.Path)
		}
		if err := w.writeData(n, strings.NewReader(e.Target), int64(len(e.Target))); err != nil {
			return fmt.Errorf("%s: %w", e.Path, err)
		}
	case TypeCharDevice, TypeBlockDevice:
		n.rdev = encodeDev(e.Major, e.Minor)
	}
	n.parent = parent
	n.links = 1
	parent.children[path.Base(p)] = n
	return nil
}

// Link adds a hard link at p to what is at target, which must be there
// already and not be a directory.
func (w *Writer) Link(p, target string) error {
	if w.done {
		return errors.New("erofs: the image is closed")
	}
	p, err := cleanPath(p)
	if err != nil {
		return err
	}
	t, err := cleanPath(target)
	if err != nil {
		return err
	}
	n := w.lookup(t)
	switch {
	case n == nil:
		return fmt.Errorf("%s: a link to %s, which is not there", p, target)
	case n.children != nil:
		return fmt.Errorf("%s: a link to a directory, %s", p, target)
	case w.lookup(p) != nil:
		return fmt.Errorf("%s: added twice", p)
	}
	parent, err := w.parentOf(p)
	if err != nil {
		return err
	}
	parent.children[path.Base(p)] = n
	n.links++
	return nil
}

// writeData writes a file's whole blocks where the data so far ends. The
// rest is kept, to go with its inode, if the two fit in one block;
// otherwise it gets a block of its own after the others.
func (w *Writer) writeData(n *node, r io.Reader, size int64) error {
	n.size = size
	n.layout = layoutPlain
	whole := size / BlockSize
	rest := size % BlockSize
	inline := rest > 0 && extendedSize+int64(len(n.xattrs))+rest <= BlockSize
	if whole > 0 || (rest > 0 && !inline) {
		n.start = uint32(w.next)
	}
	left := whole * BlockSize
	if f, ok := w.out.(*os.File); ok && left > 0 {
		if src, ok := r.(*os.File); ok {
			// File to file: the kernel copies it (copy_file_range on Linux).
			if _, err := f.Seek(w.next*BlockSize, io.SeekStart); err != nil {
				return err
			}
			if _, err := io.CopyN(f, src, left); err != nil {
				return fmt.Errorf("reading its data: %w", err)
			}
			w.next += whole
			left = 0
		}
	}
	for left > 0 {
		k := min(int64(len(w.buf)), left)
		if _, err := io.ReadFull(r, w.buf[:k]); err != nil {
			return fmt.Errorf("reading its data: %w", err)
		}
		if _, err := w.out.WriteAt(w.buf[:k], w.next*BlockSize); err != nil {
			return err
		}
		w.next += k / BlockSize
		left -= k
	}
	if rest == 0 {
		return w.checkBlocks()
	}
	if inline {
		n.layout = layoutInline
		n.tailOff = len(w.tails)
		w.tails = append(w.tails, make([]byte, rest)...)
		if _, err := io.ReadFull(r, w.tails[n.tailOff:]); err != nil {
			return fmt.Errorf("reading its data: %w", err)
		}
		n.tailLen = int(rest)
		return nil
	}
	block := w.buf[:BlockSize]
	clear(block)
	if _, err := io.ReadFull(r, block[:rest]); err != nil {
		return fmt.Errorf("reading its data: %w", err)
	}
	if _, err := w.out.WriteAt(block, w.next*BlockSize); err != nil {
		return err
	}
	w.next++
	return w.checkBlocks()
}

func (w *Writer) checkBlocks() error {
	if w.next >= nullAddr {
		return errors.New("the image is larger than EROFS's 32-bit block numbers reach")
	}
	return nil
}

// encodeXattrs is an inode's inline attribute body, and its
// i_xattr_icount; none for no attributes.
func encodeXattrs(attrs map[string][]byte) ([]byte, uint16, error) {
	if len(attrs) == 0 {
		return nil, 0, nil
	}
	names := make([]string, 0, len(attrs))
	for name := range attrs {
		names = append(names, name)
	}
	sort.Strings(names)
	body := make([]byte, xattrHeaderSize)
	for _, name := range names {
		index, suffix, ok := splitXattr(name)
		if !ok {
			continue
		}
		value := attrs[name]
		if len(suffix) > maxName {
			return nil, 0, fmt.Errorf("the attribute name %q is too long", name)
		}
		if len(value) > maxXattrValue {
			return nil, 0, fmt.Errorf("the attribute %s is larger than %d bytes", name, maxXattrValue)
		}
		e := make([]byte, align(int64(4+len(suffix)+len(value)), xattrEntrySize))
		e[0], e[1] = uint8(len(suffix)), index
		le.PutUint16(e[2:], uint16(len(value)))
		copy(e[4:], suffix)
		copy(e[4+len(suffix):], value)
		body = append(body, e...)
	}
	if len(body) == xattrHeaderSize {
		return nil, 0, nil
	}
	icount := (len(body)-xattrHeaderSize)/xattrEntrySize + 1
	if icount > 0xFFFF {
		return nil, 0, errors.New("too many extended attributes")
	}
	return body, uint16(icount), nil
}

func splitXattr(name string) (uint8, string, bool) {
	for _, p := range xattrPrefixes {
		if p.index == 2 || p.index == 3 {
			if name == p.prefix {
				return p.index, "", true
			}
			continue
		}
		if suffix, ok := strings.CutPrefix(name, p.prefix); ok && suffix != "" {
			return p.index, suffix, true
		}
	}
	return 0, "", false
}

// Size is the image's length, once it is closed: a whole number of blocks.
func (w *Writer) Size() int64 { return w.size }

// Close lays out and writes what describes the tree -- inodes,
// directories, and the data kept with them -- after the files' data, and
// the superblock before it.
func (w *Writer) Close() error {
	if w.done {
		return nil
	}
	w.done = true

	// Inodes in breadth-first order, so a directory's are near each other.
	order := []*node{w.root}
	w.root.queued = true
	for i := 0; i < len(order); i++ {
		n := order[i]
		if n.children == nil {
			continue
		}
		n.layoutDir()
		for _, d := range n.dirents {
			if c := d.n; !c.queued {
				c.queued = true
				order = append(order, c)
			}
		}
	}

	// Inodes go in the metadata area, each with its attributes and any data
	// kept with it in one block, which the kernel requires of the data.
	metaStart := w.next
	var alloc metaAlloc
	for i, n := range order {
		rec := extendedSize + int64(len(n.xattrs))
		if n.layout == layoutInline {
			rec += n.inlineLen()
		}
		n.nid = uint64(alloc.alloc(align(rec, slotSize)) >> slotBits)
		n.ino = uint32(i + 1)
	}
	if w.root.nid > 0xFFFF {
		return errors.New("the root's inode is beyond where the superblock can point")
	}

	// Directories' blocks follow the metadata area.
	next := metaStart + alloc.blocks
	for _, n := range order {
		if n.children == nil {
			continue
		}
		blocks := n.size / BlockSize
		if n.layout == layoutPlain && n.size%BlockSize != 0 {
			blocks++
		}
		if blocks > 0 {
			n.start = uint32(next)
			next += blocks
		}
	}
	if next >= nullAddr {
		return errors.New("the image is larger than EROFS's 32-bit block numbers reach")
	}

	meta := make([]byte, alloc.blocks*BlockSize)
	for _, n := range order {
		off := int64(n.nid) << slotBits
		n.encodeInode(meta[off:])
		off += extendedSize
		copy(meta[off:], n.xattrs)
		off += int64(len(n.xattrs))
		if n.children == nil {
			if n.layout == layoutInline {
				copy(meta[off:], w.tails[n.tailOff:n.tailOff+n.tailLen])
			}
			continue
		}
		data := n.renderDir()
		whole := n.size / BlockSize * BlockSize
		if n.layout == layoutInline {
			copy(meta[off:], data[whole:])
			data = data[:whole]
		}
		if len(data) > 0 {
			if _, err := w.out.WriteAt(data, int64(n.start)*BlockSize); err != nil {
				return err
			}
		}
	}
	if _, err := w.out.WriteAt(meta, metaStart*BlockSize); err != nil {
		return err
	}

	sb := make([]byte, superSize)
	le.PutUint32(sb[0:], superMagic)
	sb[12] = blockBits
	le.PutUint16(sb[14:], uint16(w.root.nid))
	le.PutUint64(sb[16:], uint64(len(order)))
	le.PutUint32(sb[36:], uint32(next))
	le.PutUint32(sb[40:], uint32(metaStart))
	if _, err := w.out.WriteAt(sb, superOffset); err != nil {
		return err
	}
	w.size = next * BlockSize
	return nil
}

// inlineLen is the length of the data kept with the inode.
func (n *node) inlineLen() int64 {
	if n.children != nil {
		return n.dirTail
	}
	return int64(n.tailLen)
}

// layoutDir sorts a directory's entries, "." and ".." among them, as the
// kernel's binary search expects, and packs them into blocks: each block
// its entries' 12-byte records, then their names.
func (n *node) layoutDir() {
	n.dirents = append(n.dirents[:0], dirent{".", n}, dirent{"..", n.parent})
	for name, c := range n.children {
		n.dirents = append(n.dirents, dirent{name, c})
	}
	sort.Slice(n.dirents, func(i, j int) bool { return n.dirents[i].name < n.dirents[j].name })
	n.dirCuts = []int{0}
	used := int64(0)
	for i, d := range n.dirents {
		need := int64(direntSize + len(d.name))
		if used+need > BlockSize {
			n.dirCuts = append(n.dirCuts, i)
			used = 0
		}
		used += need
	}
	n.size = int64(len(n.dirCuts)-1)*BlockSize + used
	n.dirTail = n.size % BlockSize
	n.start = nullAddr
	n.layout = layoutPlain
	if n.dirTail > 0 && extendedSize+int64(len(n.xattrs))+n.dirTail <= BlockSize {
		n.layout = layoutInline
	}
}

// renderDir is a directory's contents, every block but the last whole.
func (n *node) renderDir() []byte {
	out := make([]byte, align(n.size, BlockSize))
	cuts := append(n.dirCuts, len(n.dirents))
	for b := 0; b+1 < len(cuts); b++ {
		block := out[int64(b)*BlockSize:]
		ents := n.dirents[cuts[b]:cuts[b+1]]
		nameOff := len(ents) * direntSize
		for i, d := range ents {
			r := block[i*direntSize:]
			le.PutUint64(r[0:], d.n.nid)
			le.PutUint16(r[8:], uint16(nameOff))
			r[10] = direntTypes[d.n.typ()]
			nameOff += copy(block[nameOff:], d.name)
		}
	}
	return out[:n.size]
}

// nlink is how many directory entries name it: a directory's own, its
// parent's, and each of its subdirectories' "..".
func (n *node) nlink() uint32 {
	if n.children == nil {
		return n.links
	}
	links := uint32(2)
	for _, c := range n.children {
		if c.children != nil {
			links++
		}
	}
	return links
}

func (n *node) encodeInode(b []byte) {
	le.PutUint16(b[0:], versionExtended|uint16(n.layout)<<1)
	le.PutUint16(b[2:], n.icount)
	le.PutUint16(b[4:], n.mode)
	le.PutUint64(b[8:], uint64(n.size))
	switch n.typ() {
	case TypeCharDevice, TypeBlockDevice:
		le.PutUint32(b[16:], n.rdev)
	case TypeRegular, TypeDir, TypeSymlink:
		le.PutUint32(b[16:], n.start)
	}
	le.PutUint32(b[20:], n.ino)
	le.PutUint32(b[24:], n.uid)
	le.PutUint32(b[28:], n.gid)
	le.PutUint64(b[32:], uint64(n.mtime))
	le.PutUint32(b[40:], n.nsec)
	le.PutUint32(b[44:], n.nlink())
}

// metaAlloc places records in the metadata area, each within one block
// where it fits in one: in the fullest block with room for it, so few
// slots go spare.
type metaAlloc struct {
	blocks int64
	// free holds blocks by how many 32-byte slots they have left.
	free [BlockSize/slotSize + 1][]int64
}

func (a *metaAlloc) alloc(size int64) int64 {
	const slots = BlockSize / slotSize
	if size > BlockSize {
		start := a.blocks
		a.blocks += align(size, BlockSize) / BlockSize
		return start * BlockSize
	}
	need := size / slotSize
	for s := need; s <= slots; s++ {
		if l := a.free[s]; len(l) > 0 {
			b := l[len(l)-1]
			a.free[s] = l[:len(l)-1]
			off := b*BlockSize + (slots-s)*slotSize
			if s > need {
				a.free[s-need] = append(a.free[s-need], b)
			}
			return off
		}
	}
	b := a.blocks
	a.blocks++
	if need < slots {
		a.free[slots-need] = append(a.free[slots-need], b)
	}
	return b * BlockSize
}
