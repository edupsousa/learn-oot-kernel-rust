<!-- SPDX-License-Identifier: GPL-2.0 -->
# Lesson 5: `05-gdb`

Debug with GDB: `vm-debug`, `lx-symbols`, breakpoints (plain and conditional), watchpoints, `finish`/`bt`,
`lx-dmesg` / `lx-ps` / `lx-lsmod`. Uses `/dev/gdbtarget`, a Collatz module written to be debugged.

Back to the [lessons index](../README.md) (setup, VM helpers and dependencies are there).

## Kernel config

Needs `DEBUG_INFO_DWARF5` and `GDB_SCRIPTS`, and **gdb** installed.

## Run

`gdb_target` creates `/dev/gdbtarget`: write a number and it runs the Collatz sequence, remembering the steps and peak.
Three `#[no_mangle]` + `#[inline(never)]` helpers (`gdbt_parse`, `gdbt_run_collatz`, `gdbt_collatz_step`) give GDB
readable symbols to break on.
```
# terminal 1
kmake && vm-debug gdb_target.ko
# terminal 2 (from this directory)
gdb -x debug.gdb                # loads vmlinux + vmlinux-gdb.py, connects to :1234
(gdb) continue                  # boot; when the VM shell appears, press Ctrl-C in GDB
(gdb) lx-symbols .              # load gdb_target's symbols (directory containing the .ko)
(gdb) break gdbt_run_collatz
(gdb) continue
# terminal 1: exec 3<>/dev/gdbtarget; echo 27 >&3; cat <&3     -> GDB stops
```

## Exercises

1. `bt`, `info args`, `print *state`, `finish` (returns the step count).
2. Conditional breakpoint: `break gdbt_collatz_step if n == 9232` (27's sequence peaks at 9232), then `continue`.
3. Watchpoint: at `gdbt_run_collatz`, `watch -l state.last_steps` and `continue`; GDB stops on each update. Use `.`, not `->`: in Rust `state` is a reference, and `->` is a syntax error.
4. Error path: `break gdbt_parse`, then `echo abc > /dev/gdbtarget` and `step` to the `EINVAL` return.
5. Kernel helpers: `lx-dmesg`, `lx-ps` (find the shell running your write), `lx-lsmod`. Run these from a *C* frame: stopped inside module code they fail with `No symbol 'init_task'` (or `'void'`), because symbol lookup is scoped to the Rust frame. Use `bt`, then `frame N` on a kernel frame such as `vfs_write`, run the command, and `frame 0` to go back.
6. Stretch: reproduce a lesson 4 bug (`echo uaf > /dev/broken`) and catch it with `break kasan_report`.

## Troubleshooting

All of these were hit while verifying the lesson:
- Run `gdb -x debug.gdb` from this directory. `lx-symbols` scans the current directory as well as its arguments, so starting GDB from `/` or `$HOME` makes it crawl the filesystem and fail. `lx-symbols .` or an absolute path to `05-gdb` both work; module symbols are matched by `.ko` file name.
- `No symbol 'init_task'` right after starting means GDB has no `vmlinux` (`file $KDIR/vmlinux`), or you are stopped inside module code (see exercise 5).
- The "auto-loading has been declined by your auto-load safe-path" warning is harmless: `debug.gdb` sources `vmlinux-gdb.py` explicitly. Pass `-iex 'add-auto-load-safe-path $KDIR'` to silence it.
- `warning: (Internal error: pc ... in read in CU, but not in symtab.)` printed by `lx-ps` / `lx-dmesg` is harmless.
- Arguments shown as `<optimized out>` are normal at `-O2`; `finish`, `up` and `info args` one frame earlier still work.
- The module must be the one the kernel loaded: rebuild and reboot after every change, or breakpoints land at the wrong addresses.
