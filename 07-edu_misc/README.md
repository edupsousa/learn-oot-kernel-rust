<!-- SPDX-License-Identifier: GPL-2.0 -->
# Lesson 7: `07-edu_misc`

`/dev/edu` on top of the edu driver: an ioctl starts a factorial on the card and the caller sleeps on a
`CondVar` until the interrupt wakes it. Process vs interrupt context, `Atomic`, global lock.
Includes a userspace test, `edu_test.c`.

Back to the [lessons index](../README.md) (setup, VM helpers and dependencies are there).

## Run

```
kmake
musl-gcc -static -o edu_test edu_test.c      # or any static x86_64 C compiler (e.g. nixpkgs#pkgsStatic.stdenv.cc)
./vm-edu edu_misc.ko edu_test
# in the VM:
ls -l /dev/edu                   # created by the driver once the card is probed
/mods/edu_test                   # 5!, 12!, 1000 requests in a row, and the error cases
dmesg | grep edu_misc            # probe / unbind
rmmod edu_misc; ls /dev/edu      # the node disappears with the driver
```
Each ioctl writes `n` to the card and sleeps; the card raises an interrupt when done, the handler stores the result and wakes the sleeper. The per-request log line is a `dev_dbg!` (silent by default, 1000 requests would flood the console); change it to `dev_info!` to see every `n! = result`.

## Things to notice in `edu_misc.rs`

- The interrupt handler cannot take a `Mutex`, so the flag it sets is an `Atomic` (with `Release`/`Acquire` ordering) and the waiter re-checks it in a loop around `CondVar::wait_interruptible_timeout`. Rust v6.18 has no IRQ-safe lock type, and `Completion` can only wait uninterruptibly and cannot be reset, so this is the pattern available.
- That wait is not perfectly airtight: if the interrupt lands between the flag check and the moment the task is queued on the `CondVar`, the wake-up is missed. The 2 s timeout turns that rare case into a delay instead of a hang. Lesson 9 (threaded handler) removes the problem by letting the wake-up side take the mutex.
- `MiscDevice::open` receives no driver data, so the shared state goes through a `global_lock!` static that `probe` fills and `unbind` clears.
- Outside `probe` there is no `Bound` device, so the ioctl reaches the registers with `Devres::try_access`, which returns `None` after the card is removed.

Ideas to continue: remove the missed-wake-up window (hint: lesson 9), add a second ioctl that returns the number of interrupts handled, try `Ctrl-C` on a request while the card is slow (give `WAIT_TIMEOUT_MS` a tiny value to see `ETIMEDOUT`).
