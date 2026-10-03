Fixes carried against the upstream kernel, as `NNNN-name.patch`, applied in
name order by `scripts/build.sh`. Each must apply cleanly: a rejected hunk
means the fix has landed upstream or the code has moved under it, and the
build stops rather than produce a kernel nobody can predict.
