<!-- SPDX-License-Identifier: GPL-2.0 -->
# Lesson 1: `01-hello`

Build/load cycle: `module!`, `kernel::Module`, `Drop`, `pr_info!`, `insmod`/`rmmod`, `dmesg`, Kbuild files.

Back to the [lessons index](../README.md) (setup, VM helpers and dependencies are there).

## Run

```
kmake && vm-run hello.ko
# in the VM:
dmesg | tail
rmmod hello; dmesg | tail
```

`dmesg` shows "Hello from Rust in the kernel!", `lsmod` lists the module, `rmmod hello` logs "Goodbye!".
