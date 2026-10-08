<!-- SPDX-License-Identifier: GPL-2.0 -->
# Lesson 4: `04-broken`

Break it on purpose: lockdep (ABBA deadlock), sleeping in atomic context, and KASAN out-of-bounds /
use-after-free from `unsafe` code.

Back to the [lessons index](../README.md) (setup, VM helpers and dependencies are there).

## Kernel config

Needs a debug kernel: `KASAN_GENERIC` (inline), `PROVE_LOCKING`, `DEBUG_MUTEXES` and `DEBUG_ATOMIC_SLEEP`.
Rebuild the kernel and the module after any config change.

## Run

Each command in `/dev/broken` triggers one bug; read the report with `dmesg`:
```
kmake && vm-run broken.ko
echo abba  > /dev/broken   # lockdep: circular locking dependency
echo sleep > /dev/broken   # BUG: sleeping function called from invalid context
echo oob   > /dev/broken   # KASAN: out-of-bounds read
echo uaf   > /dev/broken   # KASAN: use-after-free
```
KASAN reports only the first error per boot, so reboot between `oob` and `uaf` (or add `kasan_multi_shot` to the kernel command line).
