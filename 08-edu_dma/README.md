<!-- SPDX-License-Identifier: GPL-2.0 -->
# Lesson 8: `08-edu_dma`

DMA with the `edu` card: the card copies a buffer between our RAM and its internal buffer by itself and
interrupts when done. Coherent DMA memory (`CoherentAllocation`), bus addresses, `DmaMask`, and who owns a
buffer that hardware can write to.

Back to the [lessons index](../README.md) (setup, VM helpers and dependencies are there).

## Run

```
cd 08-edu_dma
kmake && ./vm-edu edu_dma.ko
# in the VM:
dmesg | grep edu_dma
rmmod edu_dma; dmesg | tail
```
Expected, at load (each transfer takes about 100 ms, the card's own DMA delay):
```
edu_dma: probe
edu_dma: buffer: cpu 0xffff888009c2f000, bus address 0x9c2f000 (must fit in 28 bits)
edu_dma: copied 256 bytes RAM -> card
edu_dma: copied 256 bytes card -> RAM
edu_dma: round trip OK, data identical
```
The driver fills a 256-byte buffer with a pattern, DMAs it to the card, overwrites the buffer with `0xaa`, DMAs
the card's copy back, and compares. A transfer that did nothing cannot pass, because the buffer was wiped in between.
`grep edu_dma /proc/interrupts` shows two interrupts, one per transfer.

## Things to notice in `edu_dma.rs`

- **CPU address vs bus address.** `CoherentAllocation` gives a CPU pointer and `dma_handle()`, the address the *card*
  must use. Here they differ (`0xffff8880…` vs `0x9c2f000`); on a machine with an IOMMU the bus address can look
  unrelated to the physical one. Never hand the card a CPU pointer.
- **`DmaMask` first.** `edu` can only generate 28-bit addresses (`edu.rst`), so `probe` calls
  `dma_set_mask_and_coherent(DmaMask::new::<28>())` before allocating. The bus address printed above is below
  `0x1000_0000`. The call is `unsafe` because it must not race with other DMA allocations.
- **Coherent memory** is the simplest DMA memory: CPU and device always see each other's writes, no cache
  maintenance in the driver. Streaming mappings (map/unmap per transfer) need `SGTable` in v6.18; the sample
  `samples/rust/rust_dma.rs` shows it.
- **The CPU accessors are `unsafe`** (`write`, `as_slice`, `as_slice_mut`): Rust cannot know whether the device is
  touching the buffer. The safety comments in `dma_round_trip` state the rule: the CPU owns the buffer before a
  transfer starts and after the completion interrupt; the card owns it in between.
- **A failed transfer leaks the buffer on purpose** (`abandon`). After a timeout the card may still write into it;
  freeing it would let the kernel reuse that memory while hardware scribbles on it. A small leak beats silent corruption.
- **`request_irq` does not register anything by itself.** It returns a `PinInit`; the handler is installed when the
  driver data containing it is built. The first version of this lesson ran the DMA before that and always timed out
  after 2 s with the card showing `irq_status=0x100` and the handler never entered. Build the driver data first.
- The wait for completion is the lesson 7 pattern (`Atomic` flag, `CondVar`, timeout), here in `probe`.

## Card registers used (from `edu.rst`)

| Offset | Meaning |
|--------|---------|
| `0x80` | DMA source address (64-bit) |
| `0x88` | DMA destination address (64-bit) |
| `0x90` | byte count (64-bit) |
| `0x98` | command: bit 0 start (the card clears it when done), bit 1 direction (0 RAM to card, 1 card to RAM), bit 2 raise IRQ `0x100` when done |

The card side of a transfer is an address in `0x40000..0x41000` (its private 4 KiB buffer); the other side is a bus
address of our RAM.

Ideas to continue: make the buffer 4096 bytes and do a longer transfer; try `DmaMask::new::<32>()` and a bigger
RAM size to see why the mask matters (QEMU `-m`, `QEMU_EXTRA`); expose the transfer as an ioctl on a misc device by
combining this with lesson 7; do the same with an `SGTable` streaming mapping.
