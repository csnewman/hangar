import type { EnvironmentLogName } from './api'

// logNames are the logs a worker keeps of each environment, from its last
// start, in the order they are offered.
export const logNames: { name: EnvironmentLogName; label: string; about: string }[] = [
  {
    name: 'console',
    label: 'Console',
    about: "The machine's serial console: the kernel, Hangar's init assembling the root, and systemd.",
  },
  { name: 'worker', label: 'Worker', about: 'What its worker logged about it: each step of starting, and any error.' },
  { name: 'monitor', label: 'Monitor', about: "Cloud Hypervisor's own log, the virtual machine's monitor." },
  { name: 'gpu', label: 'GPU', about: "The virtual GPU's backend, for an environment with one." },
]
