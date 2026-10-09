<!-- SPDX-License-Identifier: GPL-2.0 -->
# Learning Rust Linux Kernel Modules

Small hands-on lessons on out-of-tree Linux kernel module development in Rust. Each lesson is a
self-contained module in its own numbered folder (`01-hello`, `02-counter`, ...), built against a kernel checkout and tested in a QEMU/KVM VM.
Reference code: `$KDIR/samples/rust/` in the kernel tree.

Tested with: Linux **v6.18**, rustc **1.91.0**, bindgen **0.72.1**, clang/LLVM 21.

## Lessons

| # | Folder | What it covers |
|---|--------|----------------|
| 1 | [`01-hello`](01-hello/README.md) | Build/load cycle: `module!`, `kernel::Module`, `Drop`, `pr_info!`, `insmod`/`rmmod`, `dmesg`, Kbuild files. |
| 2 | [`02-counter`](02-counter/README.md) | Module state: kernel `Mutex<T>`, pin-init (`#[pin_data]`, `try_pin_init!`), `InPlaceModule`, `PinnedDrop`, fallible allocation (`KVec`), failing `init`. Module parameters are not available in Rust on v6.18. |
| 3 | [`03-hello_dev`](03-hello_dev/README.md) | `/dev/hello` misc character device: `MiscDevice` + `#[vtable]`, per-open state, `read`/`write`, `ioctl`, safe user memory access, locking. Includes a userspace test, `ioctl_test.c`. |
| 4 | [`04-broken`](04-broken/README.md) | Break it on purpose: lockdep (ABBA deadlock), sleeping in atomic context, and KASAN out-of-bounds / use-after-free from `unsafe` code. Needs a debug kernel config. |
| 5 | [`05-gdb`](05-gdb/README.md) | Debug with GDB: `vm-debug`, `lx-symbols`, breakpoints (plain and conditional), watchpoints, `finish`/`bt`, `lx-dmesg` / `lx-ps` / `lx-lsmod`. Uses `/dev/gdbtarget`, a Collatz module written to be debugged. |
| 6 | [`06-edu_drv`](06-edu_drv/README.md) | PCI driver for QEMU's emulated `edu` card: ID table, `probe`/`unbind`, BAR mapping with `Devres`, MMIO, shared interrupt handler. |
| 7 | [`07-edu_misc`](07-edu_misc/README.md) | `/dev/edu` on top of the edu driver: an ioctl starts a factorial on the card and the caller sleeps on a `CondVar` until the interrupt wakes it. Process vs interrupt context, `Atomic`, global lock. Includes a userspace test, `edu_test.c`. |
| 8 | [`08-edu_dma`](08-edu_dma/README.md) | DMA with the `edu` card: coherent buffer (`CoherentAllocation`), `DmaMask`, bus addresses, RAM to card and back, completion interrupt. Who owns a buffer shared with hardware. |

## Dependencies

The environment is a Nix devShell (flake) that provides everything below; outside Nix, install the equivalents.

- **Linux kernel source** v6.18 (a mutable checkout, not a distro package), path in `$KDIR`.
- **rustc 1.91.0** with `rust-src`, `rustfmt`, `clippy` (the kernel supports only a window of rustc versions;
  v6.18 needs at least 1.78 and newer compilers such as 1.99 fail on its target spec).
- **LLVM/clang + lld** (kernel built with `LLVM=1`), **bindgen** and **libclang**, **GNU make**, `bc`, `flex`, `bison`, `libelf`, `openssl`.
- **QEMU** (`qemu-system-x86_64`) with **KVM** (`/dev/kvm`); without it QEMU falls back to slow emulation.
- **busybox** (static), `cpio`, `gzip`: used to build the throwaway initramfs.
- **gdb** (lesson 5) and a **static C compiler** such as musl-gcc (lesson 3's `ioctl_test`).

### VM helper scripts

The lessons assume these commands exist (defined in the workspace `flake.nix`):

- `kmake`: `make LLVM=1` with workarounds for nixpkgs' clang wrapper and an lld/objtool crash. Plain `make LLVM=1` may work on other setups.
- `vm-run [file ...]`: boots `$KDIR/arch/x86/boot/bzImage` with a busybox initramfs. Any `.ko` passed is `insmod`ed at boot and every file is copied to `/mods/`; you get a shell (Ctrl-A X quits).
- `vm-debug`: same, paused with a GDB stub on `:1234`.
- `vm-edu` (in `06-edu_drv`, `07-edu_misc` and `08-edu_dma`): wrapper that runs `vm-run` with QEMU's `-device edu` added.

## Setup

1. Get the kernel and check out the tag:
   ```
   git clone --depth 1 --branch v6.18 https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git
   export KDIR=$PWD/linux
   ```
2. Configure and build it (in `$KDIR`):
   ```
   kmake defconfig rust.config kvm_guest.config
   kmake menuconfig        # enable: CONFIG_RUST, DEBUG_INFO_DWARF5, GDB_SCRIPTS, KASAN, PROVE_LOCKING
   kmake rustavailable     # must report Rust is available
   kmake -j$(nproc)
   ```
   Lesson 4 needs `KASAN_GENERIC` (inline), `PROVE_LOCKING`, `DEBUG_MUTEXES` and `DEBUG_ATOMIC_SLEEP`.
   Lesson 5 needs `DEBUG_INFO_DWARF5` and `GDB_SCRIPTS`. Rebuild the modules after any config change,
   since a `.ko` only loads on the exact kernel it was built against.
3. `KDIR` must be set (the Nix devShell exports it, defaulting to `./linux` in the directory where the shell starts).
   The module `Makefile`s share `common.mk` and stop with an error if `KDIR` is unset; override per command with `kmake KDIR=/other/tree`.

## Running the lessons

All lessons follow the same cycle: build, boot the VM with the module, inspect `dmesg`. Each lesson folder has its own
`README.md` with the exact commands, expected output and exercises.

```
cd 01-hello && kmake && vm-run hello.ko
# in the VM:
dmesg | tail
rmmod hello; dmesg | tail
```

Notes:
- In the VM, modules sit at `/mods/<name>.ko`: `insmod /mods/x.ko` takes the path, `rmmod x` takes the module name.
- The VM holds a copy of the `.ko` made at boot, so rebuild and reboot to try a change.
- `kmake clean` removes build output; `.gitignore` already excludes it.

## IDE support (optional)

`kmake rust-analyzer` inside a module folder generates `rust-project.json`, which
rust-analyzer reads (point `rust-analyzer.linkedProjects` at it; disable `checkOnSave`, since plain `cargo check` cannot build kernel code).

## Glossary

- **Module (`.ko`)**: code loaded into the running kernel with `insmod`, removed with `rmmod`.
- **Kbuild**: the kernel build system; `obj-m` lists the modules to build.
- **`pr_info!`**: kernel logging (like `printk`); read it with `dmesg`.
- **Pin**: a promise that a value will not move in memory.
- **GFP flags**: tell the allocator how it may behave (`GFP_KERNEL` may sleep).
- **Guard**: value returned by `lock()`; holding it means holding the lock.

## License

GPL-2.0-only, see [LICENSE](LICENSE). Linux kernel modules must be GPL-compatible, and lessons 3, 4, 6 and 8
build on GPL-licensed kernel samples (`samples/rust/`).
