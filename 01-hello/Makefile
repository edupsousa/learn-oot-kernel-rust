# SPDX-License-Identifier: GPL-2.0
#
# A module is built *by the kernel's build system*, using the same compiler and flags as the
# kernel itself, so the result matches the kernel it will be loaded into. This Makefile only
# pulls in the shared rules from `learn/common.mk`, which run, in essence:
#
#   make -C $KDIR M=$PWD
#
# i.e. "enter the kernel tree at $KDIR, and build the external module in this directory ($PWD)".
# `kmake` (a wrapper around `make LLVM=1`) is the command you type: `kmake` builds, and
# `kmake clean` removes the build output.

include $(dir $(lastword $(MAKEFILE_LIST)))../common.mk
