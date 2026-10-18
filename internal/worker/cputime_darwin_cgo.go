//go:build darwin && cgo

package worker

/*
#include <mach/mach.h>
#include <mach/mach_host.h>

static int cpu_ticks(unsigned long long *busy, unsigned long long *total) {
	host_cpu_load_info_data_t info;
	mach_msg_type_number_t count = HOST_CPU_LOAD_INFO_COUNT;
	if (host_statistics(mach_host_self(), HOST_CPU_LOAD_INFO, (host_info_t)&info, &count) != KERN_SUCCESS)
		return -1;
	unsigned long long idle = info.cpu_ticks[CPU_STATE_IDLE];
	*total = idle + info.cpu_ticks[CPU_STATE_USER] + info.cpu_ticks[CPU_STATE_SYSTEM] + info.cpu_ticks[CPU_STATE_NICE];
	*busy = *total - idle;
	return 0;
}
*/
import "C"

// cpuTimes is the machine's cumulative CPU ticks, busy and in all.
func cpuTimes() cpuSample {
	var busy, total C.ulonglong
	if C.cpu_ticks(&busy, &total) != 0 {
		return cpuSample{}
	}
	return cpuSample{busy: uint64(busy), total: uint64(total)}
}
