<!-- SPDX-License-Identifier: GPL-2.0 -->
# Lesson 6: `06-edu_drv`

PCI driver for QEMU's emulated `edu` card: ID table, `probe`/`unbind`, BAR mapping with `Devres`, MMIO,
shared interrupt handler.

Back to the [lessons index](../README.md) (setup, VM helpers and dependencies are there).

## Run

```
kmake && ./vm-edu edu_drv.ko
# in the VM:
dmesg | grep edu_drv             # probe, id 0x010000ed, liveness, "irq #1: reason=0xab01 factorial=3628800"
grep edu_drv /proc/interrupts    # the IRQ is registered
grep edu_drv /proc/iomem         # BAR0 reserved by the driver
rmmod edu_drv; dmesg | tail
```
`vm-edu` wraps `vm-run` with QEMU's `-device edu` added. `edu` is the emulated hardware (PCI `1234:11e8`);
`edu_drv` is our driver for it.

Ideas to continue: DMA with `samples/rust/rust_dma.rs`, a threaded IRQ handler, a KUnit test.
