# Learning Rust Linux Kernel Modules

Small hands-on lessons on out-of-tree Linux kernel module development in Rust. Each lesson is a
self-contained module in `foundations/`, built against a kernel checkout and tested in a QEMU/KVM VM.
Reference code: `$KDIR/samples/rust/` in the kernel tree.

Tested with: Linux **v6.18**, rustc **1.91.0**, bindgen **0.72.1**, clang/LLVM 21.

## Lessons

| # | Folder | What it covers |
|---|--------|----------------|
| 1 | `foundations/01-hello` | Build/load cycle: `module!`, `kernel::Module`, `Drop`, `pr_info!`, `insmod`/`rmmod`, `dmesg`, Kbuild files. |
| 2 | `foundations/02-counter` | Module state: kernel `Mutex<T>`, pin-init (`#[pin_data]`, `try_pin_init!`), `InPlaceModule`, `PinnedDrop`, fallible allocation (`KVec`), failing `init`. Module parameters are not available in Rust on v6.18. |
| 3 | `foundations/03-hello_dev` | `/dev/hello` misc character device: `MiscDevice` + `#[vtable]`, per-open state, `read`/`write`, `ioctl`, safe user memory access, locking. Includes a userspace test, `ioctl_test.c`. |
| 4 | `foundations/04-broken` | Break it on purpose: lockdep (ABBA deadlock), sleeping in atomic context, and KASAN out-of-bounds / use-after-free from `unsafe` code. Needs a debug kernel config. |
| 5 | uses `foundations/03-hello_dev` | Debug with GDB: `vm-debug`, `lx-symbols`, breakpoints in module code, `lx-dmesg` / `lx-ps` / `lx-lsmod`. No separate folder. |
| 6 | `foundations/06-edu_drv` | PCI driver for QEMU's emulated `edu` card: ID table, `probe`/`unbind`, BAR mapping with `Devres`, MMIO, shared interrupt handler. |

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
- `06-edu_drv/vm-edu`: wrapper that runs `vm-run` with QEMU's `-device edu` added.

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

All lessons follow the same cycle: build, boot the VM with the module, inspect `dmesg`.

```
cd foundations/01-hello && kmake && vm-run hello.ko
# in the VM:
dmesg | tail
rmmod hello; dmesg | tail
```

Notes:
- In the VM, modules sit at `/mods/<name>.ko`: `insmod /mods/x.ko` takes the path, `rmmod x` takes the module name.
- The VM holds a copy of the `.ko` made at boot, so rebuild and reboot to try a change.
- `kmake clean` removes build output; `.gitignore` already excludes it.

### Lesson 1: `01-hello`
Boot with `hello.ko`; `dmesg` shows "Hello from Rust in the kernel!", `lsmod` lists it, `rmmod hello` logs "Goodbye!".

### Lesson 2: `02-counter`
`vm-run counter.ko`, then `rmmod counter`: expect `counter: exit, ticks=3 history=[1, 4, 9]`.

### Lesson 3: `03-hello_dev`
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

### Lesson 4: `04-broken`
Needs the debug kernel config above. Each command in `/dev/broken` triggers one bug; read the report with `dmesg`:
```
vm-run broken.ko
echo abba  > /dev/broken   # lockdep: circular locking dependency
echo sleep > /dev/broken   # BUG: sleeping function called from invalid context
echo oob   > /dev/broken   # KASAN: out-of-bounds read
echo uaf   > /dev/broken   # KASAN: use-after-free
```
KASAN reports only the first error per boot, so reboot between `oob` and `uaf` (or add `kasan_multi_shot` to the kernel command line).

### Lesson 5: GDB (uses `03-hello_dev`)
```
# terminal 1 (in 03-hello_dev, after building)
vm-debug hello_dev.ko ioctl_test
# terminal 2
gdb $KDIR/vmlinux -ex 'target remote :1234'
(gdb) continue                       # boot; when the shell appears, press Ctrl-C in GDB
(gdb) source $KDIR/vmlinux-gdb.py    # the symlink at the kernel root, not scripts/gdb/
(gdb) lx-symbols /path/to/learn/foundations/03-hello_dev
(gdb) info functions write_iter      # Rust symbols are mangled; find ours, then `break` on it
(gdb) continue
# terminal 1: echo hi > /dev/hello   -> GDB stops in our code
(gdb) bt
(gdb) lx-dmesg
```
Troubleshooting: `No symbol 'init_task'` means GDB started without `vmlinux` (run `file $KDIR/vmlinux`, then `lx-symbols` again).

### Lesson 6: `06-edu_drv`
```
kmake && ./vm-edu edu_drv.ko
# in the VM:
dmesg | grep edu_drv             # probe, id 0x010000ed, liveness, "irq #1: reason=0xab01 factorial=3628800"
grep edu_drv /proc/interrupts    # the IRQ is registered
grep edu_drv /proc/iomem         # BAR0 reserved by the driver
rmmod edu_drv; dmesg | tail
```
`edu` is the emulated hardware (QEMU's `-device edu`, PCI `1234:11e8`); `edu_drv` is our driver for it.
Ideas to continue: DMA with `samples/rust/rust_dma.rs`, a misc device exposing the factorial, a threaded IRQ handler, a KUnit test.

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

The repository is under the [MIT License](LICENSE). The exception is any file that carries its own
`SPDX-License-Identifier` header: the lesson sources are `GPL-2.0`, as Linux kernel modules must be
GPL-compatible, and lessons 3, 4 and 6 build on GPL-licensed kernel samples (`samples/rust/`).
