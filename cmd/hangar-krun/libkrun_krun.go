//go:build krun

package main

/*
#cgo LDFLAGS: -lkrun
#include <stdlib.h>
#include <string.h>
#include <libkrun.h>
#include <libkrun_display.h>

typedef struct {
	char *buf;
	size_t len;
} hk_buf;

static bool hk_push(void *self, KrunStr s) {
	hk_buf *b = self;
	char *n = realloc(b->buf, b->len + s.len + 1);
	if (!n)
		return false;
	memcpy(n + b->len, s.data, s.len);
	b->len += s.len;
	n[b->len] = 0;
	b->buf = n;
	return true;
}

static KrunPushStrVtable hk_push_vtable = {.drop = NULL, .push = hk_push};

// hk_error is an error's message, which the caller frees, and destroys the
// error.
static char *hk_error(KrunError e) {
	hk_buf b = {0};
	KrunVtableHandle w = KRUN_VTABLE_HANDLE(KRUN_PUSH_STR_TYPE_TAG, hk_push_vtable, &b);
	krun_error_message(e, &w);
	krun_error_destroy(e);
	return b.buf;
}

static KrunStr hk_str(const char *s) { return KRUN_STR(s); }

// A display that shows nothing: the guest's desktop is drawn by its own
// compositor and reached over VNC, and the GPU device needs a display to
// exist. A scanout the guest sets up anyway is given a frame to draw into,
// one per scanout, which is never shown.
enum { hk_scanouts = 16 };
static uint8_t *hk_frames[hk_scanouts];
static size_t hk_sizes[hk_scanouts];

static int32_t hk_disable_scanout(void *instance, uint32_t scanout) { return 0; }

static int32_t hk_configure_scanout(void *instance, uint32_t scanout, uint32_t display_width,
                                    uint32_t display_height, uint32_t width, uint32_t height, uint32_t format) {
	if (scanout >= hk_scanouts)
		return KRUN_DISPLAY_ERR_INVALID_SCANOUT_ID;
	free(hk_frames[scanout]);
	hk_sizes[scanout] = (size_t)width * height * 4;
	hk_frames[scanout] = calloc(1, hk_sizes[scanout] ? hk_sizes[scanout] : 1);
	return hk_frames[scanout] ? 0 : KRUN_DISPLAY_ERR_INTERNAL;
}

static int32_t hk_alloc_frame(void *instance, uint32_t scanout, uint8_t **buffer, size_t *size) {
	if (scanout >= hk_scanouts || !hk_frames[scanout])
		return KRUN_DISPLAY_ERR_INVALID_SCANOUT_ID;
	*buffer = hk_frames[scanout];
	*size = hk_sizes[scanout];
	return 0;
}

static int32_t hk_present_frame(void *instance, uint32_t scanout, uint32_t frame, const struct krun_rect *damage) {
	return 0;
}

static struct krun_display_backend hk_display = {
	.features = KRUN_DISPLAY_FEATURE_BASIC_FRAMEBUFFER,
	.vtable.basic_framebuffer = {
		.disable_scanout = hk_disable_scanout,
		.configure_scanout = hk_configure_scanout,
		.alloc_frame = hk_alloc_frame,
		.present_frame = hk_present_frame,
	},
};

static KrunDisplayBackend hk_display_backend(KrunError *e) {
	return krun_display_backend_new(&hk_display, sizeof hk_display, e);
}

static KrunBytes hk_bytes(const uint8_t *data, size_t len) {
	return (KrunBytes){.data = data, .len = len};
}
*/
import "C"

import (
	"errors"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"runtime"
	"unsafe"

	"github.com/creack/pty"
	"golang.org/x/sys/unix"

	"github.com/csnewman/hangar/internal/krun"
)

// maxDiskQueues bounds a disk's queues, as ch's does: past a few the guest
// gains little, and each is a thread.
const maxDiskQueues = 8

// virglrenderer's flags (virglrenderer.h), which libkrun passes on;
// surfaceless with Hangar's libkrun fork (dev/krun-build.sh).
const (
	virglUseEGL          = 1 << 0
	virglThreadSync      = 1 << 1
	virglUseSurfaceless  = 1 << 3
	virglVenus           = 1 << 6
	virglNoVirgl         = 1 << 7
	virglAsyncFenceCB    = 1 << 8
	virglRenderServer    = 1 << 9
	netFeaturesOffloaded = 1<<0 | 1<<1 | 1<<7 | 1<<10 | 1<<11 | 1<<14
)

// hasGPU is whether libkrun was built with a GPU: 2.x has a display
// backend only with one.
func hasGPU() bool {
	var e C.KrunError
	d := C.hk_display_backend(&e)
	if e != nil {
		C.free(unsafe.Pointer(C.hk_error(e)))
		return false
	}
	C.krun_display_backend_destroy(d)
	return true
}

func cstr(s string) C.KrunStr {
	// The strings are libkrun's for as long as the process lives.
	return C.hk_str(C.CString(s))
}

// failed is the error libkrun returned in e, if it returned one.
func failed(what string, e C.KrunError) error {
	if e == nil {
		return nil
	}
	msg := C.hk_error(e)
	defer C.free(unsafe.Pointer(msg))
	return fmt.Errorf("%s: %s", what, C.GoString(msg))
}

// start runs the machine c describes under libkrun until the guest exits,
// then exits the process.
func start(c krun.Config) error {
	var e C.KrunError
	level := C.uint32_t(C.KRUN_LOG_LEVEL_WARN)
	if l := os.Getenv("HANGAR_KRUN_LOG"); l != "" {
		fmt.Sscan(l, &level)
	}
	// -1 is standard error, which the worker keeps as the monitor's log.
	if C.krun_init_log(-1, level, C.KRUN_LOG_STYLE_NEVER, 0, &e); e != nil {
		return failed("logging", e)
	}

	devices, add := deviceManager()

	// The guest's console, hvc0, to the console file. libkrun makes a port
	// the console only with a terminal behind it, so it is given a
	// pseudo-terminal, whose output is copied to the file; the guest reads
	// nothing from it.
	console, err := os.OpenFile(c.Console, os.O_WRONLY|os.O_CREATE|os.O_APPEND, 0o644)
	if err != nil {
		return err
	}
	ptm, tty, err := pty.Open()
	if err != nil {
		return fmt.Errorf("the console: %w", err)
	}
	// Raw, so the guest's output reaches the file as it wrote it.
	if t, err := unix.IoctlGetTermios(int(tty.Fd()), getTermios); err == nil {
		t.Oflag &^= unix.OPOST
		t.Lflag &^= unix.ECHO | unix.ICANON
		unix.IoctlSetTermios(int(tty.Fd()), setTermios, t)
	}
	go io.Copy(console, ptm)
	// For debugging a guest by hand: what is written to the FIFO named by
	// HANGAR_KRUN_CONSOLE_IN is typed at its console. The worker never sets
	// it.
	if in := os.Getenv("HANGAR_KRUN_CONSOLE_IN"); in != "" {
		go func() {
			for {
				f, err := os.Open(in)
				if err != nil {
					return
				}
				io.Copy(ptm, f)
				f.Close()
			}
		}()
	}
	cb := C.krun_console_device_builder()
	var port C.uint32_t
	if C.krun_console_builder_add_tty_port(cb, cstr(""), C.int(tty.Fd()), &port, &e); e != nil {
		return failed("the console", e)
	}
	cons := C.krun_console_builder_build(cb, &e)
	if e != nil {
		return failed("the console", e)
	}
	add(unsafe.Pointer(cons))

	for _, d := range c.Disks {
		blk := C.krun_block_device_new(cstr(d.ID), cstr(d.Path), C.KRUN_DISK_FORMAT_RAW, &e)
		if e != nil {
			return failed("disk "+d.ID, e)
		}
		C.krun_block_device_set_read_only(blk, C.bool(d.ReadOnly))
		C.krun_block_device_set_direct_io(blk, C.bool(d.Direct))
		// Relaxed flushes reach the host's buffers; on macOS they do not
		// wait for the drive, as libkrun's own default there.
		C.krun_block_device_set_sync_mode(blk, C.KRUN_SYNC_MODE_RELAXED)
		// A queue per vCPU, each with a worker of its own, as Cloud
		// Hypervisor gives a disk, and on Linux, where every disk here is a
		// raw image, a ring per queue reading and writing straight between
		// the image and guest memory. Where there is no ring, reads and
		// writes are served by one pool of threads the queues share.
		C.krun_block_device_set_num_queues(blk, C.uint16_t(min(c.CPUs, maxDiskQueues)))
		C.krun_block_device_set_io_uring(blk, true)
		C.krun_block_device_set_parallel_reads(blk, true)
		C.krun_block_device_set_parallel_writes(blk, true)
		add(unsafe.Pointer(blk))
	}

	// vsock without TSI, libkrun's socket redirection, which needs its own
	// patched guest kernel: the guest's sockets are its own.
	vs := C.krun_vsock_device_new(3, 0, &e)
	if e != nil {
		return failed("vsock", e)
	}
	for port, sock := range c.HostPorts {
		C.krun_vsock_device_add_unix_port(vs, C.uint32_t(port), cstr(sock), false)
	}
	for port, sock := range c.GuestPorts {
		os.Remove(sock)
		C.krun_vsock_device_add_unix_port(vs, C.uint32_t(port), cstr(sock), true)
	}
	add(unsafe.Pointer(vs))

	if c.Net != nil {
		mac, err := net.ParseMAC(c.Net.MAC)
		if err != nil || len(mac) != 6 {
			return fmt.Errorf("the MAC address %q", c.Net.MAC)
		}
		cmac := C.hk_bytes((*C.uint8_t)(C.CBytes(mac)), 6)
		var nd C.KrunNetDevice
		if c.Net.Stream {
			// passt, which takes the offloads virtio-net asks for.
			nd = C.krun_net_device_new_unixstream_path(cstr("net0"), cstr(c.Net.Socket), cmac,
				netFeaturesOffloaded, 0, &e)
		} else {
			nd = C.krun_net_device_new_unixgram_path(cstr("net0"), cstr(c.Net.Socket), cmac,
				0, C.KRUN_NET_FLAGS_VFKIT, &e)
		}
		if e != nil {
			return failed("the network", e)
		}
		add(unsafe.Pointer(nd))
	}

	if c.GPU != nil {
		// Vulkan alone, through Venus in virglrenderer's render server,
		// beside hangar-krun where it is bundled.
		flags := C.uint32_t(virglVenus | virglNoVirgl | virglRenderServer | virglThreadSync | virglAsyncFenceCB)
		if exe, err := os.Executable(); err == nil && os.Getenv("RENDER_SERVER_EXEC_PATH") == "" {
			if server := filepath.Join(filepath.Dir(exe), "virgl_render_server"); fileExists(server) {
				os.Setenv("RENDER_SERVER_EXEC_PATH", server)
				renderServerEnv(filepath.Dir(exe))
			}
		}
		if c.GPU.Virgl {
			// OpenGL and Vulkan, rendered as the Linux GPU backend renders
			// them: EGL with no display, so a host without one renders on
			// its CPU, and Venus in virglrenderer's render server.
			flags = virglUseEGL | virglUseSurfaceless | virglThreadSync | virglAsyncFenceCB |
				virglVenus | virglRenderServer
		}
		d := C.hk_display_backend(&e)
		if e != nil {
			return failed("the GPU's display", e)
		}
		g := C.krun_gpu_device_new(flags, d)
		if g == nil {
			return errors.New("the GPU: this libkrun has none")
		}
		C.krun_gpu_device_shm_size(&g, C.size_t(c.GPU.WindowMiB)<<20)
		// The guest's pages are the host's (a 16 KiB kernel on the Mac), so
		// its blobs need no aligning, and its Mesa (before 26.2) can't create
		// a Venus instance when asked to align them.
		C.krun_gpu_device_blob_alignment(&g, false)
		add(unsafe.Pointer(g))
	}

	// Memory the guest frees goes back to the host.
	bal := C.krun_balloon_device_new(&e)
	if e != nil {
		return failed("the balloon", e)
	}
	add(unsafe.Pointer(bal))
	if err := addMemory(c, add); err != nil {
		return err
	}
	rng := C.krun_rng_device_new(&e)
	if e != nil {
		return failed("the random number generator", e)
	}
	add(unsafe.Pointer(rng))

	format := C.uint32_t(C.KRUN_KERNEL_FORMAT_RAW)
	if runtime.GOARCH == "amd64" {
		format = C.KRUN_KERNEL_FORMAT_ELF
	}
	payload := C.krun_payload_load_external(cstr(c.Kernel), format, cstr(c.Initrd), cstr(c.Cmdline), &e)
	if e != nil {
		return failed("loading the kernel", e)
	}

	b := C.krun_vmm_builder_new()
	if C.krun_vmm_builder_vcpus(&b, C.uint8_t(c.CPUs), &e); e != nil {
		return failed("sizing the machine", e)
	}
	if C.krun_vmm_builder_ram_mib(&b, C.uint32_t(c.MemoryMiB), &e); e != nil {
		return failed("sizing the machine", e)
	}
	// An x86 guest finds its devices, PCI and the GPU's virtio-mmio alike,
	// in the ACPI tables; the kernel has no command-line virtio-mmio devices.
	if runtime.GOARCH == "amd64" {
		if C.krun_vmm_builder_acpi(&b, true, &e); e != nil {
			return failed("describing the machine", e)
		}
	}
	C.krun_vmm_builder_payload(&b, payload)
	C.krun_vmm_builder_devices(&b, devices)
	if c.Restore != "" {
		if C.krun_vmm_builder_restore(&b, cstr(c.Restore), &e); e != nil {
			return failed("restoring the machine", e)
		}
	}
	vmm := C.krun_vmm_builder_build(&b, &e)
	if e != nil {
		return failed("building the machine", e)
	}
	handle := C.krun_vmm_handle(vmm, &e)
	if e != nil {
		return failed("getting the machine's handle", e)
	}
	suspend = func(dir string) error {
		var e C.KrunError
		if C.krun_vmm_handle_snapshot(handle, cstr(dir), &e); e != nil {
			err := failed("taking the snapshot", e)
			// A snapshot that failed leaves the machine paused.
			e = nil
			if C.krun_vmm_handle_resume(handle, &e); e != nil {
				return fmt.Errorf("%w; and %w", err, failed("resuming", e))
			}
			return err
		}
		return nil
	}
	C.krun_vmm_run(vmm)
	os.Exit(0)
	return nil
}

func fileExists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}
