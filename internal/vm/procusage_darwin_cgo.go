//go:build darwin && cgo

package vm

/*
#include <libproc.h>
#include <mach/mach_time.h>

static int hangar_usage(int pid, unsigned long long *cpu_ns, unsigned long long *footprint) {
	struct rusage_info_v2 ri;
	if (proc_pid_rusage(pid, RUSAGE_INFO_V2, (rusage_info_t *)&ri) != 0)
		return -1;
	mach_timebase_info_data_t tb;
	mach_timebase_info(&tb);
	*cpu_ns = (ri.ri_user_time + ri.ri_system_time) * tb.numer / tb.denom;
	*footprint = ri.ri_phys_footprint;
	return 0;
}
*/
import "C"

// usage is a process's CPU time, in nanoseconds, and its physical
// footprint: for hangar-krun, nearly all of it the guest's memory.
func usage(pid int) (cpuNs, footprint uint64) {
	var c, f C.ulonglong
	if C.hangar_usage(C.int(pid), &c, &f) != 0 {
		return 0, 0
	}
	return uint64(c), uint64(f)
}

// processTicks is a process's user and system CPU time, in clock ticks.
func processTicks(pid int) uint64 {
	ns, _ := usage(pid)
	return ns / (1e9 / clockTicks)
}

// guestRAM is the host memory a monitor's guest takes up: the footprint of
// hangar-krun, whose guest memory is its own.
func guestRAM(pid int) int64 {
	_, f := usage(pid)
	return int64(f)
}
