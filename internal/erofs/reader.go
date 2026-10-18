package erofs

import (
	"errors"
	"fmt"
	"io"
	"io/fs"
	"path"
	"strings"
	"time"
)

// Image is an EROFS image to read: uncompressed, as Writer and mkfs.erofs
// make by default.
type Image struct {
	r          io.ReaderAt
	blockBits  uint8
	metaStart  int64
	xattrStart int64
	rootNid    uint64
	epoch      int64
	fixedNsec  uint32
}

// The incompatible features that move where things are, which this reader
// does not follow.
const (
	incompat48Bit   = 0x80
	incompatMetabox = 0x100
)

// Open reads an image's superblock.
func Open(r io.ReaderAt) (*Image, error) {
	sb := make([]byte, superSize)
	if _, err := r.ReadAt(sb, superOffset); err != nil {
		return nil, fmt.Errorf("reading the superblock: %w", err)
	}
	if le.Uint32(sb[0:]) != superMagic {
		return nil, errors.New("not an EROFS image")
	}
	bits := sb[12]
	if bits < 9 || bits > 16 {
		return nil, fmt.Errorf("a block size of 2^%d", bits)
	}
	if f := le.Uint32(sb[80:]) & (incompat48Bit | incompatMetabox); f != 0 {
		return nil, fmt.Errorf("an image with features this reader does not follow (%#x)", f)
	}
	im := &Image{
		r:         r,
		blockBits: bits,
		rootNid:   uint64(le.Uint16(sb[14:])),
		epoch:     int64(le.Uint64(sb[24:])),
		fixedNsec: le.Uint32(sb[32:]),
	}
	im.metaStart = int64(le.Uint32(sb[40:])) << bits
	im.xattrStart = int64(le.Uint32(sb[44:])) << bits
	return im, nil
}

func (im *Image) blockSize() int64 { return 1 << im.blockBits }

// Inode is one thing in the image.
type Inode struct {
	im       *Image
	Nid      uint64
	Type     Type
	Perm     uint32
	UID, GID uint32
	ModTime  time.Time
	Size     int64
	Nlink    uint32
	// Major and Minor are a device's number.
	Major, Minor uint32

	layout uint8
	start  uint32
	loc    int64
	isize  int64
	xsize  int64
}

// Root is the root directory.
func (im *Image) Root() (*Inode, error) { return im.Inode(im.rootNid) }

// Inode reads the inode numbered nid.
func (im *Image) Inode(nid uint64) (*Inode, error) {
	in := &Inode{im: im, Nid: nid, loc: im.metaStart + int64(nid<<slotBits)}
	b := make([]byte, extendedSize)
	n, err := im.r.ReadAt(b, in.loc)
	if n < compactSize {
		if err == nil {
			err = io.ErrUnexpectedEOF
		}
		return nil, fmt.Errorf("reading inode %d: %w", nid, err)
	}
	format := le.Uint16(b[0:])
	in.layout = uint8(format>>1) & 0x7
	mode := le.Uint16(b[4:])
	in.Type = typeOfMode(mode)
	in.Perm = uint32(mode & 0o7777)
	var iu uint32
	if format&1 == versionExtended {
		if n < extendedSize {
			return nil, fmt.Errorf("reading inode %d: %w", nid, io.ErrUnexpectedEOF)
		}
		in.isize = extendedSize
		in.Size = int64(le.Uint64(b[8:]))
		iu = le.Uint32(b[16:])
		in.UID, in.GID = le.Uint32(b[24:]), le.Uint32(b[28:])
		in.ModTime = time.Unix(int64(le.Uint64(b[32:])), int64(le.Uint32(b[40:])))
		in.Nlink = le.Uint32(b[44:])
	} else {
		in.isize = compactSize
		in.Size = int64(le.Uint32(b[8:]))
		iu = le.Uint32(b[16:])
		in.UID, in.GID = uint32(le.Uint16(b[24:])), uint32(le.Uint16(b[26:]))
		in.ModTime = time.Unix(im.epoch+int64(le.Uint32(b[12:])), int64(im.fixedNsec))
		if in.Type != TypeDir && format&(1<<4) != 0 {
			in.Nlink = 1
		} else {
			in.Nlink = uint32(le.Uint16(b[6:]))
		}
	}
	in.xsize = xattrBodySize(le.Uint16(b[2:]))
	switch in.Type {
	case 0:
		return nil, fmt.Errorf("inode %d: a mode of %o", nid, mode)
	case TypeCharDevice, TypeBlockDevice:
		in.Major, in.Minor = decodeDev(iu)
	case TypeRegular, TypeDir, TypeSymlink:
		in.start = iu
		if in.layout != layoutPlain && in.layout != layoutInline {
			return nil, fmt.Errorf("inode %d: a data layout this reader does not follow (%d)", nid, in.layout)
		}
	}
	return in, nil
}

// Data reads the inode's contents: a file's data, a symlink's target, a
// directory's entries as they are stored.
func (in *Inode) Data() *io.SectionReader {
	return io.NewSectionReader(dataReader{in}, 0, in.Size)
}

type dataReader struct{ in *Inode }

func (d dataReader) ReadAt(p []byte, off int64) (int, error) {
	in := d.in
	if off >= in.Size {
		return 0, io.EOF
	}
	if rest := in.Size - off; int64(len(p)) > rest {
		p = p[:rest]
	}
	bs := in.im.blockSize()
	// The whole blocks, from in.start; with the inline layout, what follows
	// them is kept with the inode.
	whole := in.Size
	if in.layout == layoutInline {
		whole = (in.Size+bs-1)/bs*bs - bs
	}
	n := 0
	for len(p) > 0 {
		var k int
		var err error
		switch {
		case off < whole && in.start == nullAddr:
			k = int(min(int64(len(p)), whole-off))
			clear(p[:k])
		case off < whole:
			k = int(min(int64(len(p)), whole-off))
			k, err = in.im.r.ReadAt(p[:k], int64(in.start)<<in.im.blockBits+off)
		default:
			k, err = in.im.r.ReadAt(p, in.loc+in.isize+in.xsize+off-whole)
		}
		n += k
		off += int64(k)
		p = p[k:]
		if err != nil && len(p) > 0 {
			return n, err
		}
	}
	if off >= in.Size {
		return n, io.EOF
	}
	return n, nil
}

// Readlink is a symlink's target.
func (in *Inode) Readlink() (string, error) {
	if in.Type != TypeSymlink {
		return "", fmt.Errorf("inode %d is not a symlink", in.Nid)
	}
	b, err := io.ReadAll(in.Data())
	return string(b), err
}

// DirEntry is one entry of a directory.
type DirEntry struct {
	Name string
	Nid  uint64
	Type Type
}

var direntTypeOf = func() map[uint8]Type {
	m := map[uint8]Type{}
	for t, ft := range direntTypes {
		m[ft] = t
	}
	return m
}()

// ReadDir is a directory's entries, in the order they are stored -- by
// name -- without "." and "..".
func (in *Inode) ReadDir() ([]DirEntry, error) {
	all, err := in.dirents()
	if err != nil {
		return nil, err
	}
	out := all[:0:0]
	for _, e := range all {
		if e.Name != "." && e.Name != ".." {
			out = append(out, e)
		}
	}
	return out, nil
}

// dirents is every entry of a directory, "." and ".." among them.
func (in *Inode) dirents() ([]DirEntry, error) {
	if in.Type != TypeDir {
		return nil, fmt.Errorf("inode %d is not a directory", in.Nid)
	}
	data, err := io.ReadAll(in.Data())
	if err != nil {
		return nil, err
	}
	bs := in.im.blockSize()
	var out []DirEntry
	for start := int64(0); start < int64(len(data)); start += bs {
		block := data[start:min(start+bs, int64(len(data)))]
		if len(block) < direntSize {
			return nil, fmt.Errorf("directory %d: a short block", in.Nid)
		}
		count := int(le.Uint16(block[8:])) / direntSize
		if count == 0 || count*direntSize > len(block) {
			return nil, fmt.Errorf("directory %d: a bad first name offset", in.Nid)
		}
		for i := range count {
			r := block[i*direntSize:]
			off := int(le.Uint16(r[8:]))
			end := len(block)
			if i+1 < count {
				end = int(le.Uint16(block[(i+1)*direntSize+8:]))
			}
			if off > end || end > len(block) {
				return nil, fmt.Errorf("directory %d: a bad name offset", in.Nid)
			}
			name := block[off:end]
			if i+1 == count {
				if z := strings.IndexByte(string(name), 0); z >= 0 {
					name = name[:z]
				}
			}
			out = append(out, DirEntry{Name: string(name), Nid: le.Uint64(r[0:]), Type: direntTypeOf[r[10]]})
		}
	}
	return out, nil
}

// Xattrs are the inode's extended attributes, by their full names.
func (in *Inode) Xattrs() (map[string][]byte, error) {
	if in.xsize == 0 {
		return nil, nil
	}
	body := make([]byte, in.xsize)
	if _, err := in.im.r.ReadAt(body, in.loc+in.isize); err != nil {
		return nil, fmt.Errorf("reading inode %d's attributes: %w", in.Nid, err)
	}
	out := map[string][]byte{}
	shared := int(body[4])
	pos := xattrHeaderSize + 4*shared
	if pos > len(body) {
		return nil, fmt.Errorf("inode %d: a bad attribute header", in.Nid)
	}
	for i := range shared {
		id := le.Uint32(body[xattrHeaderSize+4*i:])
		head := make([]byte, 4)
		at := in.im.xattrStart + int64(id)*4
		if _, err := in.im.r.ReadAt(head, at); err != nil {
			return nil, err
		}
		e := make([]byte, 4+int(head[0])+int(le.Uint16(head[2:])))
		if _, err := in.im.r.ReadAt(e, at); err != nil {
			return nil, err
		}
		if err := addXattr(out, e); err != nil {
			return nil, fmt.Errorf("inode %d: %w", in.Nid, err)
		}
	}
	for pos+4 <= len(body) {
		size := 4 + int(body[pos]) + int(le.Uint16(body[pos+2:]))
		if pos+size > len(body) {
			return nil, fmt.Errorf("inode %d: an attribute past its end", in.Nid)
		}
		if err := addXattr(out, body[pos:pos+size]); err != nil {
			return nil, fmt.Errorf("inode %d: %w", in.Nid, err)
		}
		pos += int(align(int64(size), xattrEntrySize))
	}
	return out, nil
}

func addXattr(out map[string][]byte, e []byte) error {
	nameLen := int(e[0])
	prefix, ok := xattrPrefix(e[1])
	if !ok {
		return fmt.Errorf("an attribute with name index %d", e[1])
	}
	out[prefix+string(e[4:4+nameLen])] = append([]byte(nil), e[4+nameLen:]...)
	return nil
}

// maxSymlinks is how many symlinks resolving one path may pass through.
const maxSymlinks = 40

// Lookup finds what is at name, a slash-separated path from the root,
// following symlinks on the way, as the image's own root resolves them,
// and the last one too if follow.
func (im *Image) Lookup(name string, follow bool) (*Inode, error) {
	root, err := im.Root()
	if err != nil {
		return nil, err
	}
	parts := splitPath(name)
	cur := root
	links := 0
	for len(parts) > 0 {
		part := parts[0]
		parts = parts[1:]
		if cur.Type != TypeDir {
			return nil, fs.ErrNotExist
		}
		if part == ".." {
			if cur, err = im.child(cur, ".."); err != nil {
				return nil, err
			}
			continue
		}
		next, err := im.child(cur, part)
		if err != nil {
			return nil, err
		}
		if next.Type == TypeSymlink && (len(parts) > 0 || follow) {
			if links++; links > maxSymlinks {
				return nil, errors.New("too many levels of symbolic links")
			}
			target, err := next.Readlink()
			if err != nil {
				return nil, err
			}
			if strings.HasPrefix(target, "/") {
				cur = root
			}
			parts = append(splitPath(target), parts...)
			continue
		}
		cur = next
	}
	return cur, nil
}

func splitPath(p string) []string {
	var out []string
	for part := range strings.SplitSeq(p, "/") {
		if part != "" && part != "." {
			out = append(out, part)
		}
	}
	return out
}

// child is the inode a directory names name, ".." included.
func (im *Image) child(dir *Inode, name string) (*Inode, error) {
	ents, err := dir.dirents()
	if err != nil {
		return nil, err
	}
	for _, e := range ents {
		if e.Name == name {
			return im.Inode(e.Nid)
		}
	}
	return nil, fs.ErrNotExist
}

// FS is the image as an fs.FS, which follows symlinks as Lookup does;
// it is also an fs.ReadLinkFS and an fs.StatFS.
func (im *Image) FS() fs.FS { return imageFS{im} }

type imageFS struct{ im *Image }

func (f imageFS) lookup(op, name string, follow bool) (*Inode, error) {
	if !fs.ValidPath(name) {
		return nil, &fs.PathError{Op: op, Path: name, Err: fs.ErrInvalid}
	}
	in, err := f.im.Lookup(name, follow)
	if err != nil {
		return nil, &fs.PathError{Op: op, Path: name, Err: err}
	}
	return in, nil
}

func (f imageFS) Open(name string) (fs.File, error) {
	in, err := f.lookup("open", name, true)
	if err != nil {
		return nil, err
	}
	info := fileInfo{name: path.Base(name), in: in}
	if in.Type == TypeDir {
		ents, err := in.ReadDir()
		if err != nil {
			return nil, &fs.PathError{Op: "open", Path: name, Err: err}
		}
		return &dirFile{fs: f, info: info, ents: ents}, nil
	}
	return &file{info: info, SectionReader: in.Data()}, nil
}

func (f imageFS) Stat(name string) (fs.FileInfo, error) {
	in, err := f.lookup("stat", name, true)
	if err != nil {
		return nil, err
	}
	return fileInfo{name: path.Base(name), in: in}, nil
}

func (f imageFS) Lstat(name string) (fs.FileInfo, error) {
	in, err := f.lookup("lstat", name, false)
	if err != nil {
		return nil, err
	}
	return fileInfo{name: path.Base(name), in: in}, nil
}

func (f imageFS) ReadLink(name string) (string, error) {
	in, err := f.lookup("readlink", name, false)
	if err != nil {
		return "", err
	}
	if in.Type != TypeSymlink {
		return "", &fs.PathError{Op: "readlink", Path: name, Err: fs.ErrInvalid}
	}
	return in.Readlink()
}

type fileInfo struct {
	name string
	in   *Inode
}

func (i fileInfo) Name() string       { return i.name }
func (i fileInfo) Size() int64        { return i.in.Size }
func (i fileInfo) ModTime() time.Time { return i.in.ModTime }
func (i fileInfo) IsDir() bool        { return i.in.Type == TypeDir }
func (i fileInfo) Sys() any           { return i.in }

func (i fileInfo) Mode() fs.FileMode {
	m := fs.FileMode(i.in.Perm & 0o777)
	if i.in.Perm&0o4000 != 0 {
		m |= fs.ModeSetuid
	}
	if i.in.Perm&0o2000 != 0 {
		m |= fs.ModeSetgid
	}
	if i.in.Perm&0o1000 != 0 {
		m |= fs.ModeSticky
	}
	return m | typeFileMode(i.in.Type)
}

func typeFileMode(t Type) fs.FileMode {
	switch t {
	case TypeDir:
		return fs.ModeDir
	case TypeSymlink:
		return fs.ModeSymlink
	case TypeCharDevice:
		return fs.ModeDevice | fs.ModeCharDevice
	case TypeBlockDevice:
		return fs.ModeDevice
	case TypeFIFO:
		return fs.ModeNamedPipe
	case TypeSocket:
		return fs.ModeSocket
	}
	return 0
}

type file struct {
	info fileInfo
	*io.SectionReader
}

func (f *file) Stat() (fs.FileInfo, error) { return f.info, nil }
func (f *file) Close() error               { return nil }

type dirFile struct {
	fs   imageFS
	info fileInfo
	ents []DirEntry
	pos  int
}

func (d *dirFile) Stat() (fs.FileInfo, error) { return d.info, nil }
func (d *dirFile) Close() error               { return nil }
func (d *dirFile) Read([]byte) (int, error) {
	return 0, &fs.PathError{Op: "read", Path: d.info.name, Err: errors.New("is a directory")}
}

func (d *dirFile) ReadDir(n int) ([]fs.DirEntry, error) {
	rest := d.ents[d.pos:]
	if n > 0 && len(rest) == 0 {
		return nil, io.EOF
	}
	if n > 0 && n < len(rest) {
		rest = rest[:n]
	}
	d.pos += len(rest)
	out := make([]fs.DirEntry, len(rest))
	for i, e := range rest {
		out[i] = dirEntry{im: d.fs.im, e: e}
	}
	return out, nil
}

type dirEntry struct {
	im *Image
	e  DirEntry
}

func (d dirEntry) Name() string      { return d.e.Name }
func (d dirEntry) IsDir() bool       { return d.e.Type == TypeDir }
func (d dirEntry) Type() fs.FileMode { return typeFileMode(d.e.Type) }
func (d dirEntry) Info() (fs.FileInfo, error) {
	in, err := d.im.Inode(d.e.Nid)
	if err != nil {
		return nil, err
	}
	return fileInfo{name: d.e.Name, in: in}, nil
}

func (d dirEntry) String() string { return fs.FormatDirEntry(d) }
