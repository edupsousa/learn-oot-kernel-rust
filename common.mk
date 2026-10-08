# SPDX-License-Identifier: GPL-2.0
#
# Shared build rules for the out-of-tree modules. Each module Makefile just does:
#   include $(dir $(lastword $(MAKEFILE_LIST)))../common.mk
#
# KDIR must point to the configured and built kernel checkout. Set it in the
# environment (the Nix devShell does) or on the command line: kmake KDIR=...

ifeq ($(origin KDIR),undefined)
$(error KDIR is not set. Point it to your built kernel tree, e.g. `export KDIR=$$HOME/linux`)
endif

default:
	$(MAKE) -C $(KDIR) M=$$PWD

rust-analyzer:
	$(MAKE) -C $(KDIR) M=$$PWD rust-analyzer

clean:
	$(MAKE) -C $(KDIR) M=$$PWD clean

.PHONY: default rust-analyzer clean
