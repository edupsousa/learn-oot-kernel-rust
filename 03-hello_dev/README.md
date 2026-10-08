<!-- SPDX-License-Identifier: GPL-2.0 -->
# Lesson 3: `03-hello_dev`

`/dev/hello` misc character device: `MiscDevice` + `#[vtable]`, per-open state, `read`/`write`, `ioctl`,
safe user memory access, locking. Includes a userspace test, `ioctl_test.c`.

Back to the [lessons index](../README.md) (setup, VM helpers and dependencies are there).

## Run

Build the test binary statically (the VM has no libc), then boot with both files:
```
kmake
musl-gcc -static -o ioctl_test ioctl_test.c      # or any static x86_64 C compiler
vm-run hello_dev.ko ioctl_test
# in the VM:
ls -l /dev/hello
/mods/ioctl_test; dmesg | tail -20
```
Expect an ioctl value round trip of 42, `Bad address` for a bad pointer, `Not a tty` for an unknown command,
and "read back 9 bytes: hi kernel". Each `open()` has its own buffer, so `echo hi > /dev/hello; cat /dev/hello` prints nothing.
