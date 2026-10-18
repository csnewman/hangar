// Package ext4 reads an ext4 filesystem image without mounting it: its
// tree, each file's type, owner, mode and contents, and extended
// attributes. It reads what is on the disk, as a read-only mount with
// noload does, without replaying a journal a machine that did not shut
// down cleanly left behind. Files are mapped by extents, as mkfs.ext4 makes
// them, or kept inline; the older block maps are not read.
package ext4

import (
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"path"
	"slices"
	"strings"
	"time"
)

var le = binary.LittleEndian

const (
	superOffset = 1024
	superMagic  = 0xEF53
	rootIno     = 2

	incompatFiletype   = 0x0002
	incompatJournalDev = 0x0008
	incompatMetaBG     = 0x0010
	incompat64Bit      = 0x0080
	incompatEncrypt    = 0x10000
	incompatCasefold   = 0x20000
	incompatDirData    = 0x1000

	flagExtents    = 0x80000
	flagInlineData = 0x10000000

	extentMagic = 0xF30A
	xattrMagic  = 0xEA020000
	inlineSize  = 60
)

// FS is an ext4 filesystem to read.
type FS struct {
	r         io.ReaderAt
	blockSize int64
	ipg       uint32
	inodeSize int64
	descSize  int64
	gdt       int64
	filetype  bool
}

// Open reads a filesystem's superblock.
func Open(r io.ReaderAt) (*FS, error) {
	sb := make([]byte, 1024)
	if _, err := r.ReadAt(sb, superOffset); err != nil {
		return nil, fmt.Errorf("reading the superblock: %w", err)
	}
	if le.Uint16(sb[56:]) != superMagic {
		return nil, errors.New("not an ext4 filesystem")
	}
	logBlock := le.Uint32(sb[24:])
	if logBlock > 6 {
		return nil, fmt.Errorf("a block size of 2^%d", 10+logBlock)
	}
	incompat := le.Uint32(sb[96:])
	for _, f := range []struct {
		bit  uint32
		what string
	}{{incompatJournalDev, "an external journal"}, {incompatMetaBG, "meta_bg"},
		{incompatEncrypt, "encryption"}, {incompatCasefold, "casefolding"}, {incompatDirData, "dirdata"}} {
		if incompat&f.bit != 0 {
			return nil, fmt.Errorf("a filesystem with %s, which this reader does not follow", f.what)
		}
	}
	f := &FS{
		r:         r,
		blockSize: 1024 << logBlock,
		ipg:       le.Uint32(sb[40:]),
		inodeSize: 128,
		descSize:  32,
		filetype:  incompat&incompatFiletype != 0,
	}
	if le.Uint32(sb[76:]) >= 1 {
		f.inodeSize = int64(le.Uint16(sb[88:]))
	}
	if incompat&incompat64Bit != 0 {
		if d := int64(le.Uint16(sb[254:])); d >= 64 {
			f.descSize = d
		}
	}
	firstData := int64(le.Uint32(sb[20:]))
	f.gdt = (firstData + 1) * f.blockSize
	if f.ipg == 0 || f.inodeSize < 128 {
		return nil, errors.New("a bad superblock")
	}
	return f, nil
}

// Inode is one file of the filesystem.
type Inode struct {
	fs       *FS
	Ino      uint32
	Mode     fs.FileMode
	UID, GID uint32
	Size     int64
	Nlink    uint16
	ModTime  time.Time
	// Major and Minor are a device's number.
	Major, Minor uint32

	flags  uint32
	block  [inlineSize]byte
	acl    int64
	raw    []byte
	extra  int64
	extent []extent
}

type extent struct {
	logical, physical, length int64
	unwritten                 bool
}

// Inode reads inode number ino.
func (f *FS) Inode(ino uint32) (*Inode, error) {
	if ino == 0 {
		return nil, errors.New("inode 0")
	}
	group := int64((ino - 1) / f.ipg)
	index := int64((ino - 1) % f.ipg)
	desc := make([]byte, f.descSize)
	if _, err := f.r.ReadAt(desc, f.gdt+group*f.descSize); err != nil {
		return nil, fmt.Errorf("reading group %d's descriptor: %w", group, err)
	}
	table := int64(le.Uint32(desc[8:]))
	if f.descSize >= 64 {
		table |= int64(le.Uint32(desc[0x28:])) << 32
	}
	raw := make([]byte, f.inodeSize)
	if _, err := f.r.ReadAt(raw, table*f.blockSize+index*f.inodeSize); err != nil {
		return nil, fmt.Errorf("reading inode %d: %w", ino, err)
	}
	in := &Inode{fs: f, Ino: ino, raw: raw}
	mode := le.Uint16(raw[0:])
	in.Mode = fileMode(mode)
	in.UID = uint32(le.Uint16(raw[2:])) | uint32(le.Uint16(raw[120:]))<<16
	in.GID = uint32(le.Uint16(raw[24:])) | uint32(le.Uint16(raw[122:]))<<16
	in.Size = int64(le.Uint32(raw[4:])) | int64(le.Uint32(raw[108:]))<<32
	in.Nlink = le.Uint16(raw[26:])
	in.ModTime = time.Unix(int64(int32(le.Uint32(raw[16:]))), 0)
	in.flags = le.Uint32(raw[32:])
	copy(in.block[:], raw[40:40+inlineSize])
	in.acl = int64(le.Uint32(raw[104:])) | int64(le.Uint16(raw[118:]))<<32
	if f.inodeSize > 128 {
		in.extra = int64(le.Uint16(raw[128:]))
	}
	if in.Mode&fs.ModeDevice != 0 {
		// The old encoding in the first word, else the new in the second.
		if old := le.Uint32(in.block[0:]); old != 0 {
			in.Major, in.Minor = old>>8&0xff, old&0xff
		} else {
			dev := le.Uint32(in.block[4:])
			in.Major, in.Minor = (dev&0xfff00)>>8, dev&0xff|(dev>>12)&0xfff00
		}
	}
	if mode&0o170000 == 0 {
		return nil, fmt.Errorf("inode %d is not in use", ino)
	}
	return in, nil
}

// fileMode is an inode's mode as fs.FileMode.
func fileMode(m uint16) fs.FileMode {
	out := fs.FileMode(m & 0o777)
	if m&0o4000 != 0 {
		out |= fs.ModeSetuid
	}
	if m&0o2000 != 0 {
		out |= fs.ModeSetgid
	}
	if m&0o1000 != 0 {
		out |= fs.ModeSticky
	}
	switch m & 0o170000 {
	case 0o040000:
		out |= fs.ModeDir
	case 0o120000:
		out |= fs.ModeSymlink
	case 0o020000:
		out |= fs.ModeDevice | fs.ModeCharDevice
	case 0o060000:
		out |= fs.ModeDevice
	case 0o010000:
		out |= fs.ModeNamedPipe
	case 0o140000:
		out |= fs.ModeSocket
	}
	return out
}

// IsDir is whether the inode is a directory.
func (in *Inode) IsDir() bool { return in.Mode.IsDir() }

// inline is whether the inode keeps its data in itself: a fast symlink,
// or inline data.
func (in *Inode) inline() bool {
	if in.flags&flagInlineData != 0 {
		return true
	}
	return in.Mode&fs.ModeSymlink != 0 && in.flags&flagExtents == 0 && in.Size < inlineSize
}

// Data reads the inode's contents: a file's data, a symlink's target, a
// directory's blocks.
func (in *Inode) Data() (*io.SectionReader, error) {
	if in.inline() {
		data := append([]byte(nil), in.block[:min(in.Size, inlineSize)]...)
		if in.flags&flagInlineData != 0 && in.Size > inlineSize {
			attrs, err := in.Xattrs()
			if err != nil {
				return nil, err
			}
			data = append(data, attrs["system.data"]...)
		}
		if int64(len(data)) < in.Size {
			return nil, fmt.Errorf("inode %d: inline data short of its size", in.Ino)
		}
		return io.NewSectionReader(readerAt(data[:in.Size]), 0, in.Size), nil
	}
	if in.flags&flagExtents == 0 {
		if in.Size == 0 {
			return io.NewSectionReader(readerAt(nil), 0, 0), nil
		}
		return nil, fmt.Errorf("inode %d maps its blocks without extents, which this reader does not follow", in.Ino)
	}
	if in.extent == nil {
		ext, err := in.fs.extents(in.block[:], 0)
		if err != nil {
			return nil, fmt.Errorf("inode %d: %w", in.Ino, err)
		}
		in.extent = ext
	}
	return io.NewSectionReader(extentReader{in}, 0, in.Size), nil
}

type readerAt []byte

func (b readerAt) ReadAt(p []byte, off int64) (int, error) {
	if off >= int64(len(b)) {
		return 0, io.EOF
	}
	n := copy(p, b[off:])
	if n < len(p) {
		return n, io.EOF
	}
	return n, nil
}

// extents walks an extent tree from node, a node's bytes.
func (f *FS) extents(node []byte, depth int) ([]extent, error) {
	if depth > 5 {
		return nil, errors.New("an extent tree too deep")
	}
	if len(node) < 12 || le.Uint16(node[0:]) != extentMagic {
		return nil, errors.New("a bad extent header")
	}
	entries := int(le.Uint16(node[2:]))
	levels := le.Uint16(node[6:])
	if 12+12*entries > len(node) {
		return nil, errors.New("an extent node past its end")
	}
	var out []extent
	for i := range entries {
		e := node[12+12*i:]
		if levels == 0 {
			length := int64(le.Uint16(e[4:]))
			unwritten := length > 32768
			if unwritten {
				length -= 32768
			}
			out = append(out, extent{
				logical:   int64(le.Uint32(e[0:])),
				physical:  int64(le.Uint16(e[6:]))<<32 | int64(le.Uint32(e[8:])),
				length:    length,
				unwritten: unwritten,
			})
			continue
		}
		leaf := int64(le.Uint16(e[8:]))<<32 | int64(le.Uint32(e[4:]))
		child := make([]byte, f.blockSize)
		if _, err := f.r.ReadAt(child, leaf*f.blockSize); err != nil {
			return nil, err
		}
		sub, err := f.extents(child, depth+1)
		if err != nil {
			return nil, err
		}
		out = append(out, sub...)
	}
	return out, nil
}

type extentReader struct{ in *Inode }

// ReadAt reads through the extents; a hole, or an extent not yet written,
// reads as zeros.
func (e extentReader) ReadAt(p []byte, off int64) (int, error) {
	in := e.in
	bs := in.fs.blockSize
	if off >= in.Size {
		return 0, io.EOF
	}
	if rest := in.Size - off; int64(len(p)) > rest {
		p = p[:rest]
	}
	n := 0
	for len(p) > 0 {
		blk := off / bs
		within := off % bs
		k := int(min(int64(len(p)), bs-within))
		var ext *extent
		for i := range in.extent {
			x := &in.extent[i]
			if blk >= x.logical && blk < x.logical+x.length {
				ext = x
				break
			}
		}
		if ext == nil || ext.unwritten {
			clear(p[:k])
		} else {
			// The rest of the extent, at most.
			k = int(min(int64(len(p)), (ext.logical+ext.length-blk)*bs-within))
			got, err := in.fs.r.ReadAt(p[:k], (ext.physical+blk-ext.logical)*bs+within)
			if err != nil && got < k {
				return n + got, err
			}
		}
		n += k
		off += int64(k)
		p = p[k:]
	}
	if off >= in.Size {
		return n, io.EOF
	}
	return n, nil
}

// Readlink is a symlink's target.
func (in *Inode) Readlink() (string, error) {
	if in.Mode&fs.ModeSymlink == 0 {
		return "", fmt.Errorf("inode %d is not a symlink", in.Ino)
	}
	d, err := in.Data()
	if err != nil {
		return "", err
	}
	b, err := io.ReadAll(d)
	return string(b), err
}

// DirEntry is one entry of a directory.
type DirEntry struct {
	Name string
	Ino  uint32
}

// ReadDir is a directory's entries, without "." and "..", in the order
// they are stored.
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
	if !in.IsDir() {
		return nil, fmt.Errorf("inode %d is not a directory", in.Ino)
	}
	d, err := in.Data()
	if err != nil {
		return nil, err
	}
	data, err := io.ReadAll(d)
	if err != nil {
		return nil, err
	}
	var out []DirEntry
	if in.flags&flagInlineData != 0 {
		// An inline directory starts with its parent's inode, and keeps
		// no "." or "..".
		if len(data) < 4 {
			return nil, fmt.Errorf("directory %d: short inline data", in.Ino)
		}
		out = append(out, DirEntry{".", in.Ino}, DirEntry{"..", le.Uint32(data[0:])})
		ents, err := in.fs.parseDirents(data[4:min(inlineSize, int64(len(data)))])
		if err != nil {
			return nil, fmt.Errorf("directory %d: %w", in.Ino, err)
		}
		out = append(out, ents...)
		if len(data) > inlineSize {
			ents, err := in.fs.parseDirents(data[inlineSize:])
			if err != nil {
				return nil, fmt.Errorf("directory %d: %w", in.Ino, err)
			}
			out = append(out, ents...)
		}
		return out, nil
	}
	bs := in.fs.blockSize
	for start := int64(0); start < int64(len(data)); start += bs {
		ents, err := in.fs.parseDirents(data[start:min(start+bs, int64(len(data)))])
		if err != nil {
			return nil, fmt.Errorf("directory %d: %w", in.Ino, err)
		}
		out = append(out, ents...)
	}
	return out, nil
}

// parseDirents reads one block of linear directory entries. A hashed
// directory's index blocks read as one empty entry each, and a checksum's
// tail as another, both with inode 0, so they are skipped like a deleted
// entry.
func (f *FS) parseDirents(b []byte) ([]DirEntry, error) {
	var out []DirEntry
	for off := 0; off+8 <= len(b); {
		ino := le.Uint32(b[off:])
		recLen := int(le.Uint16(b[off+4:]))
		nameLen := int(b[off+6])
		if !f.filetype {
			nameLen |= int(b[off+7]) << 8
		}
		if recLen < 8 || off+recLen > len(b) || 8+nameLen > recLen {
			return nil, errors.New("a bad directory entry")
		}
		if ino != 0 && nameLen > 0 {
			out = append(out, DirEntry{Name: string(b[off+8 : off+8+nameLen]), Ino: ino})
		}
		off += recLen
	}
	return out, nil
}

// The prefixes an extended attribute's name is stored by, by index.
var xattrPrefixes = map[uint8]string{
	1: "user.",
	2: "system.posix_acl_access",
	3: "system.posix_acl_default",
	4: "trusted.",
	6: "security.",
	7: "system.",
	8: "system.richacl",
}

// Xattrs are the inode's extended attributes, by their full names: those in
// the inode and those in its attribute block.
func (in *Inode) Xattrs() (map[string][]byte, error) {
	out := map[string][]byte{}
	if start := 128 + in.extra; start+4 <= int64(len(in.raw)) && le.Uint32(in.raw[start:]) == xattrMagic {
		body := in.raw[start+4:]
		if err := parseXattrs(body, body, out); err != nil {
			return nil, fmt.Errorf("inode %d: %w", in.Ino, err)
		}
	}
	if in.acl != 0 {
		blk := make([]byte, in.fs.blockSize)
		if _, err := in.fs.r.ReadAt(blk, in.acl*in.fs.blockSize); err != nil {
			return nil, fmt.Errorf("inode %d's attribute block: %w", in.Ino, err)
		}
		if le.Uint32(blk[0:]) != xattrMagic {
			return nil, fmt.Errorf("inode %d: a bad attribute block", in.Ino)
		}
		if err := parseXattrs(blk[32:], blk, out); err != nil {
			return nil, fmt.Errorf("inode %d: %w", in.Ino, err)
		}
	}
	return out, nil
}

// parseXattrs reads entries from entries, whose values are at offsets in
// values, until the four zero bytes that end them.
func parseXattrs(entries, values []byte, out map[string][]byte) error {
	for off := 0; off+16 <= len(entries); {
		if le.Uint32(entries[off:]) == 0 {
			return nil
		}
		nameLen := int(entries[off])
		index := entries[off+1]
		valOff := int(le.Uint16(entries[off+2:]))
		valInum := le.Uint32(entries[off+4:])
		valSize := int(le.Uint32(entries[off+8:]))
		if off+16+nameLen > len(entries) {
			return errors.New("an attribute past its end")
		}
		name := xattrPrefixes[index] + string(entries[off+16:off+16+nameLen])
		if valInum == 0 {
			if valOff+valSize > len(values) {
				return errors.New("an attribute's value past its end")
			}
			out[name] = append([]byte(nil), values[valOff:valOff+valSize]...)
		}
		off += (16 + nameLen + 3) &^ 3
	}
	return nil
}

// maxSymlinks is how many symlinks resolving one path may pass through.
const maxSymlinks = 40

// Root is the root directory.
func (f *FS) Root() (*Inode, error) { return f.Inode(rootIno) }

// Lookup finds what is at name, a slash-separated path from the root,
// following symlinks on the way as the filesystem's own root resolves them,
// and the last one too if follow. A path that is not there, or passes
// through something that is not a directory, is fs.ErrNotExist.
func (f *FS) Lookup(name string, follow bool) (*Inode, error) {
	root, err := f.Root()
	if err != nil {
		return nil, err
	}
	parts := splitPath(name)
	cur := root
	links := 0
	for len(parts) > 0 {
		part := parts[0]
		parts = parts[1:]
		if !cur.IsDir() {
			return nil, fs.ErrNotExist
		}
		ents, err := cur.dirents()
		if err != nil {
			return nil, err
		}
		var ino uint32
		for _, e := range ents {
			if e.Name == part {
				ino = e.Ino
				break
			}
		}
		if ino == 0 {
			return nil, fs.ErrNotExist
		}
		next, err := f.Inode(ino)
		if err != nil {
			return nil, err
		}
		if next.Mode&fs.ModeSymlink != 0 && (len(parts) > 0 || follow) {
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

// FS is the filesystem as an fs.FS, which follows symlinks as Lookup does;
// it is also an fs.ReadLinkFS and an fs.StatFS. A file's Sys is its
// *Inode.
func (f *FS) FS() fs.FS { return fsys{f} }

type fsys struct{ f *FS }

func (s fsys) lookup(op, name string, follow bool) (*Inode, error) {
	if !fs.ValidPath(name) {
		return nil, &fs.PathError{Op: op, Path: name, Err: fs.ErrInvalid}
	}
	in, err := s.f.Lookup(name, follow)
	if err != nil {
		return nil, &fs.PathError{Op: op, Path: name, Err: err}
	}
	return in, nil
}

func (s fsys) Open(name string) (fs.File, error) {
	in, err := s.lookup("open", name, true)
	if err != nil {
		return nil, err
	}
	info := fileInfo{name: path.Base(name), in: in}
	if in.IsDir() {
		ents, err := in.ReadDir()
		if err != nil {
			return nil, &fs.PathError{Op: "open", Path: name, Err: err}
		}
		return &dirFile{f: s.f, info: info, ents: ents}, nil
	}
	if in.Mode.Type() != 0 && in.Mode&fs.ModeSymlink == 0 {
		// A device, FIFO or socket has nothing to read here.
		return &file{info: info, SectionReader: io.NewSectionReader(readerAt(nil), 0, 0)}, nil
	}
	d, err := in.Data()
	if err != nil {
		return nil, &fs.PathError{Op: "open", Path: name, Err: err}
	}
	return &file{info: info, SectionReader: d}, nil
}

func (s fsys) Stat(name string) (fs.FileInfo, error) {
	in, err := s.lookup("stat", name, true)
	if err != nil {
		return nil, err
	}
	return fileInfo{name: path.Base(name), in: in}, nil
}

func (s fsys) Lstat(name string) (fs.FileInfo, error) {
	in, err := s.lookup("lstat", name, false)
	if err != nil {
		return nil, err
	}
	return fileInfo{name: path.Base(name), in: in}, nil
}

func (s fsys) ReadLink(name string) (string, error) {
	in, err := s.lookup("readlink", name, false)
	if err != nil {
		return "", err
	}
	if in.Mode&fs.ModeSymlink == 0 {
		return "", &fs.PathError{Op: "readlink", Path: name, Err: fs.ErrInvalid}
	}
	return in.Readlink()
}

func (s fsys) ReadDir(name string) ([]fs.DirEntry, error) {
	f, err := s.Open(name)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	d, ok := f.(*dirFile)
	if !ok {
		return nil, &fs.PathError{Op: "readdir", Path: name, Err: errors.New("not a directory")}
	}
	ents, err := d.ReadDir(-1)
	if err != nil {
		return nil, err
	}
	slices.SortFunc(ents, func(a, b fs.DirEntry) int { return strings.Compare(a.Name(), b.Name()) })
	return ents, nil
}

type fileInfo struct {
	name string
	in   *Inode
}

func (i fileInfo) Name() string       { return i.name }
func (i fileInfo) Size() int64        { return i.in.Size }
func (i fileInfo) Mode() fs.FileMode  { return i.in.Mode }
func (i fileInfo) ModTime() time.Time { return i.in.ModTime }
func (i fileInfo) IsDir() bool        { return i.in.IsDir() }
func (i fileInfo) Sys() any           { return i.in }

type file struct {
	info fileInfo
	*io.SectionReader
}

func (f *file) Stat() (fs.FileInfo, error) { return f.info, nil }
func (f *file) Close() error               { return nil }

type dirFile struct {
	f    *FS
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
	out := make([]fs.DirEntry, 0, len(rest))
	for _, e := range rest {
		in, err := d.f.Inode(e.Ino)
		if err != nil {
			return out, err
		}
		out = append(out, fs.FileInfoToDirEntry(fileInfo{name: e.Name, in: in}))
	}
	return out, nil
}
