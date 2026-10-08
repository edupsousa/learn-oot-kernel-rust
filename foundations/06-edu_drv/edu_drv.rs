// SPDX-License-Identifier: GPL-2.0

//! Lesson 6: a PCI driver for QEMU's `edu` educational device.
//!
//! Two things are called "edu", do not mix them up:
//! - the **`edu` device** is emulated *hardware*: a fake PCI card that QEMU provides (`-device edu`);
//! - **`edu_drv`** (this file, `edu_drv.ko`) is our *driver*: kernel code that finds the card and
//!   talks to it.
//!
//! The `edu` device (QEMU `-device edu`, PCI id 1234:11e8) has one MMIO BAR (BAR0) with a few
//! registers: an identification register, a "liveness" register that returns the bitwise NOT of
//! what you wrote, a factorial calculator that can raise an interrupt when done, and registers to
//! raise and acknowledge interrupts by hand.
//!
//! Life cycle of a PCI driver:
//!
//! ```text
//!  PCI core finds a device whose id matches our table
//!        │
//!        ▼
//!   probe()   enable device, map BAR0, register IRQ handler, talk to the hardware
//!        │        returns the driver data, which lives until the device goes away
//!        ▼
//!   ... device in use, interrupts call EduIrq::handle ...
//!        │
//!   unbind()  device being removed (or rmmod): last chance to touch the hardware
//!        ▼
//!   drop      driver data freed, BAR unmapped, IRQ freed (automatically)
//! ```

use kernel::{
    c_str,
    device::{Bound, Core, Device},
    devres::Devres,
    irq::{self, IrqReturn},
    pci,
    prelude::*,
    sync::{
        aref::ARef,
        atomic::{Atomic, Relaxed},
        Arc,
    },
};

/// Register offsets inside BAR0 (see QEMU's `docs/specs/edu.rst`).
struct Regs;

impl Regs {
    /// Read-only: `0xRRrr00ed`, major/minor version and the fixed byte `0xed`.
    const ID: usize = 0x00;
    /// Write a value, read back its bitwise NOT: proves the device is alive.
    const LIVENESS: usize = 0x04;
    /// Write `n` to start computing `n!`; read the result once done.
    const FACTORIAL: usize = 0x08;
    /// Bit 0 = computing (read-only), bit 7 (`0x80`) = raise an IRQ when the factorial is done.
    const STATUS: usize = 0x20;
    /// Pending interrupt reasons. Non-zero means "this device is asserting its IRQ line".
    const IRQ_STATUS: usize = 0x24;
    /// Write any value to raise an interrupt with that value as the reason.
    const IRQ_RAISE: usize = 0x60;
    /// Write the bits you handled to clear them and de-assert the IRQ line.
    const IRQ_ACK: usize = 0x64;
    /// Size of the register window we use. The compile-time size lets the compiler reject
    /// accesses beyond it.
    const END: usize = 0x68;
}

// --- How to recognise the device -------------------------------------------------------------

/// PCI vendor id of the `edu` device. It is QEMU's own vendor id (the same one the QEMU VGA
/// card uses), not a real company's.
const EDU_VENDOR_ID: u16 = 0x1234;

/// PCI device id of the `edu` device, assigned by QEMU.
const EDU_DEVICE_ID: u16 = 0x11e8;

/// PCI class code (base class `0x00`, sub-class `0xff`) the device reports: "device that fits no
/// other class". The low byte (programming interface) is `0x00` and not used for matching.
const EDU_PCI_CLASS: u32 = 0x00ff00;

/// Mask for the class match: compare base class and sub-class (the top two bytes of the 24-bit
/// class code), ignore the programming-interface byte.
const EDU_PCI_CLASS_MASK: u32 = 0xffff00;

/// The `ID` register's low byte is always `0xed` ("ed" for edu); the high bytes are the
/// major/minor version (`0x01_00_00_ed` means 1.0). We only check this fixed byte.
const ID_SIGNATURE: u32 = 0xed;

/// Mask selecting the fixed signature byte of the `ID` register.
const ID_SIGNATURE_MASK: u32 = 0xff;

// --- Where things are ------------------------------------------------------------------------

/// Index of the BAR (Base Address Register) holding the device registers. `edu` has one
/// memory BAR, number 0 (1 MiB, of which we use the first `Regs::END` bytes).
const EDU_REGS_BAR: u32 = 0;

/// Index of the interrupt vector to use. Without MSI/MSI-X (not available to Rust drivers in
/// this kernel) the device has a single legacy interrupt line, which is vector 0.
const EDU_IRQ_VECTOR: u32 = 0;

// --- Values we write to the device -----------------------------------------------------------

/// Arbitrary pattern for the liveness test: the device must answer with its bitwise NOT
/// (`0xedcba987`), which proves registers can be written and read back.
const LIVENESS_TEST_VALUE: u32 = 0x1234_5678;

/// Number whose factorial we ask the device to compute. 10! = 3628800 fits the 32-bit result.
const FACTORIAL_INPUT: u32 = 10;

/// Reason value we pass to `IRQ_RAISE` to make the device interrupt by hand. The device ORs it
/// into `IRQ_STATUS`. It avoids bit 0, which the device uses for "factorial finished", so the
/// two causes can be told apart in the log.
const MANUAL_IRQ_REASON: u32 = 0xab00;

/// Value for the `STATUS` register meaning "no control bits set": do not interrupt when a
/// factorial finishes. Used at teardown to silence the device.
const STATUS_IRQ_DISABLED: u32 = 0;

/// Bit to set in the `STATUS` register to make the device raise an interrupt when a factorial
/// computation finishes. Without it the device computes silently and we would have to poll
/// `STATUS` bit 0 ("still computing") to know when the result in `FACTORIAL` is ready.
/// The interrupt it raises carries reason `0x01` in `IRQ_STATUS`.
const STATUS_IRQ_ON_FACTORIAL: u32 = 0x80;

/// A mapped view of the device's BAR0 (Base Address Register 0), the MMIO window where the
/// device's registers live: reading and writing it talks to the hardware, not to RAM.
///
/// - `pci::Bar<N>` is the kernel's checked wrapper around that mapping. `N` is the number of
///   bytes we use (`Regs::END`), known at compile time, so an access to a constant offset beyond
///   it fails to build instead of touching memory that is not ours.
/// - It offers `read8/16/32` and `write8/16/32` (and `try_` variants for offsets only known at
///   run time). Never dereference the address directly: MMIO needs special accessors so the
///   compiler and CPU do not reorder, merge or drop the accesses.
/// - Dropping it unmaps the BAR and releases the region we reserved with `iomap_region_sized`.
type Bar0 = pci::Bar<{ Regs::END }>;

/// The interrupt handler's state.
///
/// It needs the BAR to talk to the device, so the BAR is shared with the driver through an
/// `Arc` (reference counted pointer). The counter is atomic because the handler runs in
/// interrupt context, possibly concurrently with other code, and takes `&self` only: no locks
/// in interrupt context unless they are spinlocks.
#[pin_data]
struct EduIrq {
    /// Shared handle to the register window (the driver struct holds another).
    bar: Arc<Devres<Bar0>>,
    /// How many interrupts we have handled; only used for the log line.
    count: Atomic<u32>,
}

// `irq::Handler` is the trait the kernel's IRQ layer needs from whatever we register for an
// interrupt line: one method, called every time the line fires. It requires `Sync` because the
// handler may run on any CPU while other code (e.g. `unbind`) is touching the same data.
// The registration (`irq::Registration<EduIrq>`) owns an `EduIrq` and frees the IRQ on drop.
impl irq::Handler for EduIrq {
    /// Called by the kernel each time the interrupt line fires (the "hard" IRQ handler).
    ///
    /// - `&self`: our `EduIrq` state. Shared reference only, since several CPUs could enter.
    /// - `dev`: the device that owns the interrupt, in the `Bound` state: the driver is
    ///   currently attached to it, so using its resources is allowed. We pass it to
    ///   `Devres::access` as proof.
    ///
    /// Returns `Handled` if our device caused the interrupt, `None` if it was someone else's
    /// (the kernel counts "spurious" interrupts, and disables a line that nobody claims).
    ///
    /// Runs in hard interrupt context: must be fast and must not sleep.
    fn handle(&self, dev: &Device<Bound>) -> IrqReturn {
        // `Devres::access` checks the BAR still belongs to this bound device and has not been
        // revoked (device removal), and gives us the register window.
        let Ok(bar) = self.bar.access(dev) else {
            return IrqReturn::None;
        };

        // The PCI interrupt line may be shared with other devices. Only claim the interrupt
        // if our device is the one asserting it.
        let reason = bar.read32(Regs::IRQ_STATUS);
        if reason == 0 {
            return IrqReturn::None;
        }

        // Acknowledge, otherwise the (level triggered) line stays asserted and the kernel
        // would call us again forever.
        bar.write32(reason, Regs::IRQ_ACK);

        self.count.add(1, Relaxed);
        dev_info!(
            dev,
            "irq #{}: reason={:#x} factorial={}\n",
            self.count.load(Relaxed),
            reason,
            bar.read32(Regs::FACTORIAL)
        );
        IrqReturn::Handled
    }
}

/// The driver data: one instance per bound device.
#[pin_data(PinnedDrop)]
struct EduDriver {
    /// Counted reference to the PCI device, so we can name it in log messages. `ARef` keeps the
    /// underlying `struct pci_dev` alive for as long as we hold it.
    pdev: ARef<pci::Device>,
    // Field order matters for drop order: the IRQ registration goes away before the BAR.
    #[pin]
    // Never read: owning it is what keeps the interrupt registered (dropped => IRQ freed).
    _irq: irq::Registration<EduIrq>,
    /// Our own handle to the register window, used by `unbind`. Dropped last.
    bar: Arc<Devres<Bar0>>,
}

// Which devices this driver binds to. The Rust `pci::Vendor` type has no constant (and no public
// constructor) for edu's vendor id 0x1234, so we match on the PCI class "other" (base 0x00,
// subclass 0xff) and check the real ids in `probe`. Returning `ENODEV` there means "not mine".
kernel::pci_device_table!(
    PCI_TABLE,
    MODULE_PCI_TABLE,
    <EduDriver as pci::Driver>::IdInfo,
    [(pci::DeviceId::from_class(EDU_PCI_CLASS, EDU_PCI_CLASS_MASK), ())]
);

// `pci::Driver` is the trait that turns a Rust type into a PCI driver. The PCI core calls its
// methods as the device comes and goes: `probe` (required) and `unbind` (optional). There is no
// `remove` method: cleanup that does not need the hardware is just `Drop` of the driver data.
impl pci::Driver for EduDriver {
    /// Extra data attached to each entry of the id table. When the PCI core matches a device
    /// it hands us the entry's data in `probe` (e.g. "this id is variant B of the chip").
    /// We have a single kind of device, so `()`.
    type IdInfo = ();

    /// The table of devices this driver can handle (defined above). The kernel exports it in
    /// the module so `modprobe` can auto-load the driver when a matching device appears.
    const ID_TABLE: pci::IdTable<Self::IdInfo> = &PCI_TABLE;

    /// Called when the PCI core finds a device matching `ID_TABLE` (at boot, hotplug, or
    /// when our module is loaded). This is where we take over and initialise the device.
    ///
    /// - `pdev`: the PCI device, in the `Core` state: we are allowed to configure it
    ///   (enable it, set bus mastering). `Core` also lets us use everything allowed on `Bound`.
    /// - `_info`: the data of the matching id table entry (unused, see `IdInfo`).
    ///
    /// Returns the driver data (`Pin<KBox<Self>>`). The core stores it and gives it back to
    /// `unbind`; it is dropped when the device is unbound. Returning `Err` means "not bound":
    /// everything created so far is dropped, and the device stays without a driver.
    fn probe(pdev: &pci::Device<Core>, _info: &Self::IdInfo) -> Result<Pin<KBox<Self>>> {
        if pdev.vendor_id().as_raw() != EDU_VENDOR_ID || pdev.device_id() != EDU_DEVICE_ID {
            return Err(ENODEV);
        }
        dev_info!(pdev.as_ref(), "edu_drv: probe\n");

        // Turn on the device's memory decoding and bus mastering so it answers MMIO accesses.
        pdev.enable_device_mem()?;
        pdev.set_master();

        // Reserve and map BAR0. `Devres` ties the mapping to the device: it is released
        // automatically when the device is unbound. `Arc` lets the IRQ handler share it.
        let bar = Arc::pin_init(
            pdev.iomap_region_sized::<{ Regs::END }>(EDU_REGS_BAR, c_str!("edu_drv")),
            GFP_KERNEL,
        )?;

        {
            let io = bar.access(pdev.as_ref())?;

            let id = io.read32(Regs::ID);
            dev_info!(pdev.as_ref(), "edu_drv: id register = {:#010x}\n", id);
            if id & ID_SIGNATURE_MASK != ID_SIGNATURE {
                return Err(ENODEV);
            }

            io.write32(LIVENESS_TEST_VALUE, Regs::LIVENESS);
            dev_info!(
                pdev.as_ref(),
                "edu_drv: liveness: wrote {:#010x}, read {:#010x}\n",
                LIVENESS_TEST_VALUE,
                io.read32(Regs::LIVENESS)
            );
        }

        // Register the interrupt handler for the device's (only) interrupt vector. `SHARED` because PCI legacy interrupt lines can be shared between devices.
        // The initialiser closure takes ownership of what it captures, so give it its own clone.
        let irq_bar = bar.clone();
        let irq = pdev.request_irq(
            EDU_IRQ_VECTOR,
            irq::Flags::SHARED,
            c_str!("edu_drv"),
            try_pin_init!(EduIrq {
                bar: irq_bar,
                count: Atomic::new(0),
            }),
        )?;

        let drvdata = KBox::pin_init(
            try_pin_init!(Self {
                pdev: pdev.into(),
                _irq <- irq,
                bar: bar.clone(),
            }),
            GFP_KERNEL,
        )?;

        // From here on interrupts can arrive. Start a computation that interrupts when done
        // (reason bit 0x01) and raise one by hand (`MANUAL_IRQ_REASON`). `IRQ_STATUS` collects the reasons as
        // bits, and the line is level triggered, so if both happen before the handler runs you
        // get ONE interrupt with reason 0xab01. That is normal: handlers must handle all bits.
        let io = drvdata.bar.access(pdev.as_ref())?;
        io.write32(STATUS_IRQ_ON_FACTORIAL, Regs::STATUS);
        io.write32(FACTORIAL_INPUT, Regs::FACTORIAL); // computed asynchronously by the device
        io.write32(MANUAL_IRQ_REASON, Regs::IRQ_RAISE);

        Ok(drvdata)
    }

    /// Called when the driver is being detached from the device: `rmmod edu_drv`, unplugging
    /// the device, or unbinding through sysfs. It runs *before* the driver data is dropped and
    /// before devres resources (the BAR mapping, the IRQ) are released, so the hardware can
    /// still be used here. Use it for things that need the device, like silencing it.
    ///
    /// - `pdev`: the device, still `Core`.
    /// - `this`: our driver data, as returned by `probe` (pinned, so `Pin<&Self>`).
    fn unbind(pdev: &pci::Device<Core>, this: Pin<&Self>) {
        // Last moment where the hardware is still reachable: stop it from interrupting us.
        if let Ok(io) = this.bar.access(pdev.as_ref()) {
            io.write32(STATUS_IRQ_DISABLED, Regs::STATUS);
        }
        dev_info!(pdev.as_ref(), "edu_drv: unbind\n");
    }
}

// Runs after `unbind`, when the driver data is freed. Rust then drops the fields by itself
// (IRQ registration, then the `Arc` to the BAR, then the device reference); the BAR is unmapped
// once the last `Arc` and the devres entry are gone. We only add a log line.
// No hardware access here: the device may already be gone, which is why `unbind` exists.
#[pinned_drop]
impl PinnedDrop for EduDriver {
    fn drop(self: Pin<&mut Self>) {
        dev_info!(self.pdev.as_ref(), "edu_drv: remove\n");
    }
}

// Generates `module!` plus the init/exit code that registers the PCI driver with the PCI core
// at `insmod` (which immediately probes any matching device that is already present) and
// unregisters it at `rmmod` (which unbinds every device first).
kernel::module_pci_driver! {
    type: EduDriver,
    name: "edu_drv",
    authors: ["edupsousa"],
    description: "Lesson 6: QEMU edu PCI device",
    license: "GPL",
}
