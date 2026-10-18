// Package erofs writes and reads EROFS images: the read-only filesystem an
// environment's image is attached as.
//
// The images it writes are uncompressed, with 4 KiB blocks, so a guest can
// map a file's blocks straight from the host's page cache (DAX). A file's
// data is written as it arrives, so an image is built from a stream -- a tar
// archive, or a directory walked once -- without unpacking it anywhere; what
// describes the tree (inodes, directories, the last part of each small file)
// is laid out once everything has arrived, after the data. The on-disk format
// is Linux's, fs/erofs/erofs_fs.h.
package erofs

import "encoding/binary"

// BlockSize is the block size of the images Writer makes.
const BlockSize = 4096

const (
	blockBits   = 12
	superOffset = 1024
	superMagic  = 0xE0F5E1E2
	superSize   = 128

	// An inode's number (nid) is its offset from the metadata area's start
	// in 32-byte slots.
	slotBits     = 5
	slotSize     = 1 << slotBits
	compactSize  = 32
	extendedSize = 64

	// i_format: bit 0 is the inode's version, bits 1-3 its data layout.
	versionExtended = 1
	layoutPlain     = 0
	layoutInline    = 2

	nullAddr = 0xFFFFFFFF

	direntSize = 12
	maxName    = 255

	xattrHeaderSize = 12
	xattrEntrySize  = 4
	maxXattrValue   = 0xFFFF
)

// Type is the kind of thing an entry is.
type Type uint8

const (
	TypeRegular Type = iota + 1
	TypeDir
	TypeSymlink
	TypeCharDevice
	TypeBlockDevice
	TypeFIFO
	TypeSocket
)

func (t Type) String() string {
	switch t {
	case TypeRegular:
		return "file"
	case TypeDir:
		return "directory"
	case TypeSymlink:
		return "symlink"
	case TypeCharDevice:
		return "character device"
	case TypeBlockDevice:
		return "block device"
	case TypeFIFO:
		return "fifo"
	case TypeSocket:
		return "socket"
	}
	return "unknown"
}

// The S_IFMT bits of a mode, by type.
var typeModes = map[Type]uint16{
	TypeRegular:     0o100000,
	TypeDir:         0o040000,
	TypeSymlink:     0o120000,
	TypeCharDevice:  0o020000,
	TypeBlockDevice: 0o060000,
	TypeFIFO:        0o010000,
	TypeSocket:      0o140000,
}

func typeOfMode(mode uint16) Type {
	for t, m := range typeModes {
		if mode&0o170000 == m {
			return t
		}
	}
	return 0
}

// The file types a directory entry records, as Linux's FT_* values.
var direntTypes = map[Type]uint8{
	TypeRegular:     1,
	TypeDir:         2,
	TypeCharDevice:  3,
	TypeBlockDevice: 4,
	TypeFIFO:        5,
	TypeSocket:      6,
	TypeSymlink:     7,
}

// encodeDev is a device number as Linux's new_encode_dev packs it.
func encodeDev(major, minor uint32) uint32 {
	return minor&0xff | major<<8 | (minor&^0xff)<<12
}

func decodeDev(dev uint32) (major, minor uint32) {
	return (dev & 0xfff00) >> 8, dev&0xff | (dev>>12)&0xfff00
}

// The prefixes an extended attribute's name is stored by: an index and the
// rest of the name.
var xattrPrefixes = []struct {
	index  uint8
	prefix string
}{
	// The ACLs are whole names, so they come before the prefixes they share.
	{2, "system.posix_acl_access"},
	{3, "system.posix_acl_default"},
	{1, "user."},
	{4, "trusted."},
	{5, "lustre."},
	{6, "security."},
}

func xattrPrefix(index uint8) (string, bool) {
	for _, p := range xattrPrefixes {
		if p.index == index {
			return p.prefix, true
		}
	}
	return "", false
}

var le = binary.LittleEndian

func align(n, to int64) int64 { return (n + to - 1) / to * to }

// xattrBodySize is the size of an inode's inline attributes, from its
// i_xattr_icount.
func xattrBodySize(icount uint16) int64 {
	if icount == 0 {
		return 0
	}
	return xattrHeaderSize + xattrEntrySize*int64(icount-1)
}
