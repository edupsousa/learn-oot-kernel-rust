<!-- SPDX-License-Identifier: GPL-2.0 -->
# Learning Rust Linux Kernel Modules

Small hands-on lessons on out-of-tree Linux kernel module development in Rust. Each lesson is a
self-contained module in its own numbered folder (`01-hello`, `02-counter`, ...), built against a kernel checkout and tested in a QEMU/KVM VM.
Reference code: `$KDIR/samples/rust/` in the kernel tree.

Tested with: Linux **v6.18**, rustc **1.91.0**, bindgen **0.72.1**, clang/LLVM 21.

## Lessons

| # | Folder | What it covers |
|---|--------|----------------|
| 1 | `01-hello` | Build/load cycle: `module!`, `kernel::Module`, `Drop`, `pr_info!`, `insmod`/`rmmod`, `dmesg`, Kbuild files. |
| 2 | `02-counter` | Module state: kernel `Mutex<T>`, pin-init (`#[pin_data]`, `try_pin_init!`), `InPlaceModule`, `PinnedDrop`, fallible allocation (`KVec`), failing `init`. Module parameters are not available in Rust on v6.18. |
| 3 | `03-hello_dev` | `/dev/hello` misc character device: `MiscDevice` + `#[vtable]`, per-open state, `read`/`write`, `ioctl`, safe user memory access, locking. Includes a userspace test, `ioctl_test.c`. |
| 4 | `04-broken` | Break it on purpose: lockdep (ABBA deadlock), sleeping in atomic context, and KASAN out-of-bounds / use-after-free from `unsafe` code. Needs a debug kernel config. |
| 5 | `05-gdb` | Debug with GDB: `vm-debug`, `lx-symbols`, breakpoints (plain and conditional), watchpoints, `finish`/`bt`, `lx-dmesg` / `lx-ps` / `lx-lsmod`. Uses `/dev/gdbtarget`, a Collatz module written to be debugged. |
| 6 | `06-edu_drv` | PCI driver for QEMU's emulated `edu` card: ID table, `probe`/`unbind`, BAR mapping with `Devres`, MMIO, shared interrupt handler. |

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
cd 01-hello && kmake && vm-run hello.ko
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

### Lesson 5: `05-gdb`
`gdb_target` creates `/dev/gdbtarget`: write a number and it runs the Collatz sequence, remembering the steps and peak.
Three `#[no_mangle]` + `#[inline(never)]` helpers (`gdbt_parse`, `gdbt_run_collatz`, `gdbt_collatz_step`) give GDB
readable symbols to break on. Needs `DEBUG_INFO_DWARF5` and `GDB_SCRIPTS`.
```
# terminal 1
cd 05-gdb && kmake && vm-debug gdb_target.ko
# terminal 2
cd 05-gdb && gdb -x debug.gdb     # loads vmlinux + vmlinux-gdb.py, connects to :1234
(gdb) continue                  # boot; when the VM shell appears, press Ctrl-C in GDB
(gdb) lx-symbols .              # load gdb_target's symbols (directory containing the .ko)
(gdb) break gdbt_run_collatz
(gdb) continue
# terminal 1: exec 3<>/dev/gdbtarget; echo 27 >&3; cat <&3     -> GDB stops
```
Exercises:
1. `bt`, `info args`, `print *state`, `finish` (returns the step count).
2. Conditional breakpoint: `break gdbt_collatz_step if n == 9232` (27's sequence peaks at 9232), then `continue`.
3. Watchpoint: at `gdbt_run_collatz`, `watch -l state.last_steps` and `continue`; GDB stops on each update. Use `.`, not `->`: in Rust `state` is a reference, and `->` is a syntax error.
4. Error path: `break gdbt_parse`, then `echo abc > /dev/gdbtarget` and `step` to the `EINVAL` return.
5. Kernel helpers: `lx-dmesg`, `lx-ps` (find the shell running your write), `lx-lsmod`. Run these from a *C* frame: stopped inside module code they fail with `No symbol 'init_task'` (or `'void'`), because symbol lookup is scoped to the Rust frame. Use `bt`, then `frame N` on a kernel frame such as `vfs_write`, run the command, and `frame 0` to go back.
6. Stretch: reproduce a lesson 4 bug (`echo uaf > /dev/broken`) and catch it with `break kasan_report`.

Troubleshooting (all of these were hit while verifying the lesson):
- Run `gdb -x debug.gdb` from the module directory (`05-gdb`). `lx-symbols` scans the current directory as well as its arguments, so starting GDB from `/` or `$HOME` makes it crawl the filesystem and fail. `lx-symbols .` or an absolute path to `05-gdb` both work; module symbols are matched by `.ko` file name.
- `No symbol 'init_task'` right after starting means GDB has no `vmlinux` (`file $KDIR/vmlinux`), or you are stopped inside module code (see exercise 5).
- The "auto-loading has been declined by your auto-load safe-path" warning is harmless: `debug.gdb` sources `vmlinux-gdb.py` explicitly. Pass `-iex 'add-auto-load-safe-path $KDIR'` to silence it.
- `warning: (Internal error: pc ... in read in CU, but not in symtab.)` printed by `lx-ps` / `lx-dmesg` is harmless.
- Arguments shown as `<optimized out>` are normal at `-O2`; `finish`, `up` and `info args` one frame earlier still work.
- The module must be the one the kernel loaded: rebuild and reboot after every change, or breakpoints land at the wrong addresses.

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

GPL-2.0-only, see [LICENSE](LICENSE). Linux kernel modules must be GPL-compatible, and lessons 3, 4 and 6
build on GPL-licensed kernel samples (`samples/rust/`).
