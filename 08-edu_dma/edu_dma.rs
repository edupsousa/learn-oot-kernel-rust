// SPDX-License-Identifier: GPL-2.0

//! Lesson 8: DMA with the `edu` card.
//!
//! Until now the CPU moved every byte to the card with MMIO. **DMA** (Direct Memory Access) lets
//! the *card* read and write main memory by itself: we tell it "copy N bytes from here to there"
//! and it interrupts us when done. This lesson copies a buffer from RAM into the card's internal
//! 4 KiB buffer, wipes our copy, copies it back, and checks that the data survived:
//!
//! ```text
//!        RAM                                         edu card
//!   ┌──────────────┐   1. RAM -> card (DMA read)   ┌───────────────────┐
//!   │ coherent buf │ ────────────────────────────► │ internal buffer   │
//!   │  pattern     │                               │ at 0x40000        │
//!   │              │   2. wipe the buf with 0xaa   │                   │
//!   │              │   3. card -> RAM (DMA write)  │                   │
//!   │  pattern     │ ◄──────────────────────────── │                   │
//!   └──────────────┘   4. compare with the pattern └───────────────────┘
//! ```
//!
//! New ideas compared with lessons 6 and 7:
//! - **Two address spaces.** The CPU uses virtual addresses; the card uses *bus* (DMA) addresses.
//!   `CoherentAllocation` gives us both: a CPU view of the buffer and its `dma_handle()`.
//! - **The device limits where its buffer may live.** `edu` can only address 28 bits (256 MiB),
//!   so we tell the DMA layer with a `DmaMask` before allocating.
//! - **Ownership of memory shared with hardware.** While a transfer runs, the card may touch the
//!   buffer at any time. The CPU must not read or write it, and it must not be freed. This is
//!   why the CPU accessors of `CoherentAllocation` are `unsafe`.
//! - The completion interrupt wakes the waiting task, exactly as in lesson 7.
//!
//! Run it with `./vm-edu edu_dma.ko`; the README has the expected output.

use kernel::{
    c_str,
    device::{Bound, Core, Device},
    devres::Devres,
    dma::{CoherentAllocation, DmaAddress, DmaMask, Device as _},
    irq::{self, IrqReturn},
    new_condvar, new_mutex, pci,
    prelude::*,
    sync::{
        aref::ARef,
        atomic::{ordering, Atomic, Relaxed},
        Arc, CondVar, CondVarTimeoutResult, Mutex,
    },
    time::msecs_to_jiffies,
};

/// Register offsets inside BAR0 (see QEMU's `docs/specs/edu.rst`).
struct Regs;

impl Regs {
    /// Read-only: `0xRRrr00ed`, major/minor version and the fixed byte `0xed`.
    const ID: usize = 0x00;
    /// Pending interrupt reasons. Non-zero means "this device is asserting its IRQ line".
    const IRQ_STATUS: usize = 0x24;
    /// Write the bits you handled to clear them and de-assert the IRQ line.
    const IRQ_ACK: usize = 0x64;
    /// 64-bit: where the transfer reads from.
    const DMA_SRC: usize = 0x80;
    /// 64-bit: where the transfer writes to.
    const DMA_DST: usize = 0x88;
    /// 64-bit: how many bytes to copy.
    const DMA_COUNT: usize = 0x90;
    /// 64-bit: command. Writing it with `DMA_CMD_START` set begins the transfer.
    const DMA_CMD: usize = 0x98;
    /// Size of the register window we use (end of `DMA_CMD`).
    const END: usize = 0xa0;
}

// --- DMA description of the card -------------------------------------------------------------

/// Command bit: start the transfer. The card clears it when the transfer is finished.
const DMA_CMD_START: u64 = 1 << 0;
/// Command bit: direction. 0 = RAM to card, 1 = card to RAM. (Seen from the card: it either
/// *reads* RAM or *writes* RAM.)
const DMA_CMD_TO_RAM: u64 = 1 << 1;
/// Command bit: raise an interrupt (reason `IRQ_REASON_DMA_DONE`) when the transfer finishes.
const DMA_CMD_IRQ: u64 = 1 << 2;

/// Reason bit in `IRQ_STATUS` meaning "a DMA transfer finished".
const IRQ_REASON_DMA_DONE: u32 = 0x100;

/// The card has a private 4 KiB buffer. The card's DMA engine names it by *its own* address
/// space, starting at this value: it is the card-side end of every transfer, while the other
/// end is a bus address of our RAM.
const EDU_BUF_ADDR: u64 = 0x40000;
/// Size of that private buffer.
const EDU_BUF_SIZE: usize = 4096;

/// The card can only generate 28-bit DMA addresses (QEMU's `edu` sets a 256 MiB DMA mask).
/// Handing it a buffer above that would silently corrupt memory, so we declare the limit and
/// the DMA layer places our allocations below it.
const EDU_DMA_BITS: u32 = 28;

/// Size of our test buffer, in bytes. Must fit in the card's buffer.
const BUF_LEN: usize = 256;

// Compile-time check: a buffer larger than the card's own would make the transfer run past it.
const _: () = assert!(BUF_LEN <= EDU_BUF_SIZE);

/// Byte the buffer is filled with between the two transfers, so a transfer that did nothing
/// cannot be mistaken for a success.
const POISON: u8 = 0xaa;

/// How long to wait for a transfer before giving up.
const WAIT_TIMEOUT_MS: u32 = 2000;

// --- How to recognise the device (same as lesson 6) ------------------------------------------

const EDU_VENDOR_ID: u16 = 0x1234;
const EDU_DEVICE_ID: u16 = 0x11e8;
const EDU_PCI_CLASS: u32 = 0x00ff00;
const EDU_PCI_CLASS_MASK: u32 = 0xffff00;
const ID_SIGNATURE: u32 = 0xed;
const ID_SIGNATURE_MASK: u32 = 0xff;
const EDU_REGS_BAR: u32 = 0;
const EDU_IRQ_VECTOR: u32 = 0;

type Bar0 = pci::Bar<{ Regs::END }>;

// --- State shared between probe (process context) and the IRQ handler ------------------------

/// What the waiting task and the interrupt handler both need (the lesson 7 pattern).
#[pin_data]
struct Shared {
    /// The register window.
    bar: Arc<Devres<Bar0>>,
    /// Serialises transfers (the card does one at a time) and is the lock the `CondVar` needs.
    #[pin]
    request: Mutex<()>,
    /// The waiter sleeps here until `finished` becomes 1.
    #[pin]
    done: CondVar,
    /// 0 = transfer running, 1 = finished. Set by the IRQ handler, so it is atomic.
    finished: Atomic<u32>,
}

impl Shared {
    /// Run one DMA transfer and sleep until the card reports it finished.
    ///
    /// `src`/`dst` are in the card's view: one of them is a bus address of our RAM, the other
    /// is in the card's private buffer, depending on `to_ram`.
    fn transfer(
        &self,
        dev: &Device<Bound>,
        to_ram: bool,
        src: u64,
        dst: u64,
        len: usize,
    ) -> Result {
        let mut guard = self.request.lock();
        self.finished.store(0, Relaxed);

        {
            let bar = self.bar.access(dev)?;
            bar.write64(src, Regs::DMA_SRC);
            bar.write64(dst, Regs::DMA_DST);
            bar.write64(len as u64, Regs::DMA_COUNT);
            // The command write is the "go" button: the card starts copying as soon as
            // `DMA_CMD_START` is set, and interrupts us when done (`DMA_CMD_IRQ`).
            let dir = if to_ram { DMA_CMD_TO_RAM } else { 0 };
            bar.write64(DMA_CMD_START | DMA_CMD_IRQ | dir, Regs::DMA_CMD);
        }

        // Same wait loop as lesson 7: re-check the flag after every wake-up. `Acquire` pairs
        // with the handler's `Release` store. If the interrupt lands just before we sleep, the
        // wake-up is missed and the timeout turns that into a delay rather than a hang.
        let mut left = msecs_to_jiffies(WAIT_TIMEOUT_MS);
        while self.finished.load(ordering::Acquire) == 0 {
            match self.done.wait_interruptible_timeout(&mut guard, left) {
                CondVarTimeoutResult::Woken { jiffies } => left = jiffies,
                CondVarTimeoutResult::Signal { .. } => return Err(ERESTARTSYS),
                CondVarTimeoutResult::Timeout => return Err(ETIMEDOUT),
            }
        }
        Ok(())
    }
}

/// The interrupt handler's state.
#[pin_data]
struct EduIrq {
    shared: Arc<Shared>,
}

impl irq::Handler for EduIrq {
    /// Hard IRQ context: acknowledge the card, publish "finished", wake the waiter.
    fn handle(&self, dev: &Device<Bound>) -> IrqReturn {
        let Ok(bar) = self.shared.bar.access(dev) else {
            return IrqReturn::None;
        };
        let reason = bar.read32(Regs::IRQ_STATUS);
        if reason == 0 {
            return IrqReturn::None; // shared line, not our card
        }
        bar.write32(reason, Regs::IRQ_ACK);

        if reason & IRQ_REASON_DMA_DONE != 0 {
            self.shared.finished.store(1, ordering::Release);
            self.shared.done.notify_all();
        }
        IrqReturn::Handled
    }
}

// --- The driver ------------------------------------------------------------------------------

#[pin_data(PinnedDrop)]
struct EduDriver {
    pdev: ARef<pci::Device>,
    // Declared before `shared`/`bar` so the IRQ is freed first when the driver data is dropped.
    #[pin]
    _irq: irq::Registration<EduIrq>,
    shared: Arc<Shared>,
}

kernel::pci_device_table!(
    PCI_TABLE,
    MODULE_PCI_TABLE,
    <EduDriver as pci::Driver>::IdInfo,
    [(pci::DeviceId::from_class(EDU_PCI_CLASS, EDU_PCI_CLASS_MASK), ())]
);

/// The byte we put at position `i` of the test pattern: distinct per position, so a copy that
/// shifted or repeated data is detected.
fn pattern(i: usize) -> u8 {
    (i as u8) ^ 0x5a
}

impl EduDriver {
    /// Copy a pattern to the card and back through DMA, and check it.
    fn dma_round_trip(pdev: &pci::Device<Core>, shared: &Shared) -> Result {
        let dev = pdev.as_ref();

        // 1. Tell the DMA layer what the card can address. This must come before the first
        //    allocation or mapping. A failure here means this machine cannot serve the card.
        //
        // SAFETY: nothing else allocates or maps DMA memory for this device yet.
        unsafe { pdev.dma_set_mask_and_coherent(DmaMask::new::<EDU_DMA_BITS>())? };

        // 2. Allocate the buffer. "Coherent" memory is mapped so that CPU and card always see
        //    each other's writes (no cache flushing on our side), the simplest kind of DMA
        //    memory. We get a CPU view and a bus address to give to the card.
        let mut buf: CoherentAllocation<u8> =
            CoherentAllocation::alloc_coherent(dev, BUF_LEN, GFP_KERNEL)?;
        let bus_addr: DmaAddress = buf.dma_handle();
        dev_info!(
            dev,
            "edu_dma: buffer: cpu {:p}, bus address {:#x} (must fit in {} bits)\n",
            buf.start_ptr(),
            bus_addr,
            EDU_DMA_BITS
        );

        // 3. Fill it. While no transfer is running the CPU owns the buffer.
        let mut data = [0u8; BUF_LEN];
        for (i, b) in data.iter_mut().enumerate() {
            *b = pattern(i);
        }
        // SAFETY: no transfer is running (none started yet), and nothing else uses `buf`.
        unsafe { buf.write(&data, 0)? };

        // 4. RAM -> card. From here until `transfer` returns Ok the card owns the buffer.
        if let Err(e) = shared.transfer(dev, false, bus_addr, EDU_BUF_ADDR, BUF_LEN) {
            Self::abandon(dev, buf);
            return Err(e);
        }
        dev_info!(dev, "edu_dma: copied {} bytes RAM -> card\n", BUF_LEN);

        // 5. Back to CPU ownership: wipe our copy, so only the card still has the pattern.
        // SAFETY: the transfer finished (the card reported completion), so it no longer
        // touches the buffer.
        unsafe { buf.write(&[POISON; BUF_LEN], 0)? };

        // 6. card -> RAM.
        if let Err(e) = shared.transfer(dev, true, EDU_BUF_ADDR, bus_addr, BUF_LEN) {
            Self::abandon(dev, buf);
            return Err(e);
        }
        dev_info!(dev, "edu_dma: copied {} bytes card -> RAM\n", BUF_LEN);

        // 7. Check what came back.
        // SAFETY: as in step 5, the card is idle and nothing writes the buffer.
        let back = unsafe { buf.as_slice(0, BUF_LEN)? };
        if back != data {
            let bad = back.iter().zip(&data).position(|(a, b)| a != b);
            dev_err!(dev, "edu_dma: MISMATCH, first difference at {:?}\n", bad);
            return Err(EIO);
        }
        dev_info!(dev, "edu_dma: round trip OK, data identical\n");
        Ok(())
    }

    /// A transfer did not complete: the card may still write into the buffer at any moment, so
    /// freeing it could corrupt whatever the kernel reuses that memory for. Leaking a few bytes
    /// is the only safe choice.
    fn abandon(dev: &Device<Bound>, buf: CoherentAllocation<u8>) {
        dev_err!(dev, "edu_dma: transfer failed, leaking the buffer on purpose\n");
        core::mem::forget(buf);
    }
}

impl pci::Driver for EduDriver {
    type IdInfo = ();
    const ID_TABLE: pci::IdTable<Self::IdInfo> = &PCI_TABLE;

    fn probe(pdev: &pci::Device<Core>, _info: &Self::IdInfo) -> Result<Pin<KBox<Self>>> {
        if pdev.vendor_id().as_raw() != EDU_VENDOR_ID || pdev.device_id() != EDU_DEVICE_ID {
            return Err(ENODEV);
        }
        dev_info!(pdev.as_ref(), "edu_dma: probe\n");

        // The card can only DMA if it is a bus master, i.e. allowed to start bus transactions.
        pdev.enable_device_mem()?;
        pdev.set_master();

        let bar = Arc::pin_init(
            pdev.iomap_region_sized::<{ Regs::END }>(EDU_REGS_BAR, c_str!("edu_dma")),
            GFP_KERNEL,
        )?;
        if bar.access(pdev.as_ref())?.read32(Regs::ID) & ID_SIGNATURE_MASK != ID_SIGNATURE {
            return Err(ENODEV);
        }

        let shared = Arc::pin_init(
            try_pin_init!(Shared {
                bar,
                request <- new_mutex!(()),
                done <- new_condvar!(),
                finished: Atomic::new(0),
            }),
            GFP_KERNEL,
        )?;

        let irq_shared = shared.clone();
        let irq = pdev.request_irq(
            EDU_IRQ_VECTOR,
            irq::Flags::SHARED,
            c_str!("edu_dma"),
            try_pin_init!(EduIrq { shared: irq_shared }),
        )?;

        // `request_irq` only *describes* the registration (`irq` is a `PinInit`); the handler is
        // installed when the driver data below is built. So build it first: running the DMA
        // before this point would wait for an interrupt nobody is listening for.
        let drvdata = KBox::pin_init(
            try_pin_init!(Self {
                pdev: pdev.into(),
                _irq <- irq,
                shared,
            }),
            GFP_KERNEL,
        )?;

        // Run the demo. An error makes `probe` fail (so `insmod` shows it and the module does
        // not stay bound); `drvdata` is dropped and the IRQ freed on the way out.
        Self::dma_round_trip(pdev, &drvdata.shared)?;

        Ok(drvdata)
    }

    fn unbind(pdev: &pci::Device<Core>, _this: Pin<&Self>) {
        dev_info!(pdev.as_ref(), "edu_dma: unbind\n");
    }
}

#[pinned_drop]
impl PinnedDrop for EduDriver {
    fn drop(self: Pin<&mut Self>) {
        dev_info!(self.pdev.as_ref(), "edu_dma: remove\n");
    }
}

kernel::module_pci_driver! {
    type: EduDriver,
    name: "edu_dma",
    authors: ["edupsousa"],
    description: "Lesson 8: DMA with the QEMU edu PCI device",
    license: "GPL",
}
