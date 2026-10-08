# SPDX-License-Identifier: GPL-2.0
# Usage: gdb -x debug.gdb      (after `vm-debug gdb_target.ko` is waiting in another terminal)
# $KDIR must be set. Run from this directory: lx-symbols also scans the current directory.
python import os; gdb.execute("file " + os.environ["KDIR"] + "/vmlinux")
python gdb.execute("source " + os.environ["KDIR"] + "/vmlinux-gdb.py")
target remote :1234
set pagination off

# Boot the kernel. When the VM shell appears, press Ctrl-C here to get the prompt back,
# then run: lx-symbols <this directory>
echo \nType `continue`, wait for the VM shell, press Ctrl-C, then:\n
echo   lx-symbols .          (from this directory)\n
echo   break gdbt_run_collatz\n
echo   continue\n
