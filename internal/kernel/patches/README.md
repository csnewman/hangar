Changes carried against the upstream kernel, as `NNNN-name.patch`, applied in
name order by `scripts/build.sh`: the hooks that build Hangar's own code in
`../tree`, and changes to upstream code itself, such as NFS over vsock
(`0002-sunrpc-vsock.patch`), which upstream never took. Each must apply
cleanly: a rejected hunk means the code has moved under it, or the change has
landed upstream, and the build stops rather than produce a kernel nobody can
predict.
