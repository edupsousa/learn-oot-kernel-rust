// SPDX-License-Identifier: GPL-2.0

//! Lesson 7: `/dev/edu`, a misc device on top of the `edu` PCI driver, and sleeping until an
//! interrupt arrives.
//!
//! Lesson 6 poked the card from `probe`. Here a *user process* asks for work with an `ioctl`,
//! and the driver puts that process to sleep until the card says "done" with an interrupt:
//!
//! ```text
//!   process (ioctl)                     card                 hard IRQ handler
//!   ───────────────                     ────                 ────────────────
//!   take the request lock
//!   finished = 0
//!   write n to FACTORIAL  ───────────►  computes n!
//!   sleep on the CondVar                    │
//!      (CPU runs other tasks)               ▼
//!                                      raises the IRQ ─────►  read + ack IRQ_STATUS
//!                                                             store result, finished = 1
//!   woken up  ◄──────────────────────────────────────────────  notify_all()
//!   return result to user space
//! ```
//!
//! New ideas compared with lessons 3 and 6:
//! - **Two worlds share data.** The ioctl runs in *process context* (may sleep); the handler runs
//!   in *interrupt context* (must not sleep, cannot take a `Mutex`). They talk through an
//!   `Atomic` flag plus a `CondVar`.
//! - **A misc device cannot carry driver data.** `MiscDevice::open` only receives the
//!   registration, so we reach our PCI device's state through a small global (`CURRENT`).
//! - **Outside `probe` there is no `Bound` device**, so `Devres::access` is not available. The
//!   ioctl uses `try_access()` instead, which fails once the device is gone.
//!
//! Run it with `./vm-edu edu_misc.ko`, then see the README for the test commands.

use core::pin::Pin;

use kernel::{
    c_str,
    device::{Bound, Core, Device},
    devres::Devres,
    driver,
    fs::File,
    ioctl::{_IOC_SIZE, _IOWR},
    irq::{self, IrqReturn},
    miscdevice::{MiscDevice, MiscDeviceOptions, MiscDeviceRegistration},
    new_condvar, new_mutex, pci,
    prelude::*,
    sync::{
        aref::ARef,
        atomic::{ordering, Atomic, Relaxed},
        Arc, CondVar, CondVarTimeoutResult, Mutex,
    },
    time::msecs_to_jiffies,
    uaccess::{UserPtr, UserSlice},
    InPlaceModule, ModuleMetadata,
};

// --- The edu device (same as lesson 6) -------------------------------------------------------

/// Register offsets inside BAR0 (see QEMU's `docs/specs/edu.rst`).
struct Regs;

impl Regs {
    /// Read-only: `0xRRrr00ed`, major/minor version and the fixed byte `0xed`.
    const ID: usize = 0x00;
    /// Write `n` to start computing `n!`; read the result once done.
    const FACTORIAL: usize = 0x08;
    /// Bit 0 = computing (read-only), bit 7 (`0x80`) = raise an IRQ when the factorial is done.
    const STATUS: usize = 0x20;
    /// Pending interrupt reasons. Non-zero means "this device is asserting its IRQ line".
    const IRQ_STATUS: usize = 0x24;
    /// Write the bits you handled to clear them and de-assert the IRQ line.
    const IRQ_ACK: usize = 0x64;
    /// Size of the register window we use.
    const END: usize = 0x68;
}

/// QEMU's own PCI vendor id and the id it gave the `edu` device.
const EDU_VENDOR_ID: u16 = 0x1234;
const EDU_DEVICE_ID: u16 = 0x11e8;
/// PCI class "other" (base 0x00, sub-class 0xff) and the mask that ignores the last byte.
const EDU_PCI_CLASS: u32 = 0x00ff00;
const EDU_PCI_CLASS_MASK: u32 = 0xffff00;
/// The low byte of `ID` is always `0xed`.
const ID_SIGNATURE: u32 = 0xed;
const ID_SIGNATURE_MASK: u32 = 0xff;
/// BAR holding the registers, and the (only, legacy) interrupt vector.
const EDU_REGS_BAR: u32 = 0;
const EDU_IRQ_VECTOR: u32 = 0;
/// `STATUS` values: raise an interrupt when a factorial finishes / stay silent.
const STATUS_IRQ_ON_FACTORIAL: u32 = 0x80;
const STATUS_IRQ_DISABLED: u32 = 0;
/// `IRQ_STATUS` bit meaning "a factorial finished".
const IRQ_REASON_FACTORIAL_DONE: u32 = 0x01;

type Bar0 = pci::Bar<{ Regs::END }>;

// --- The ioctl interface ---------------------------------------------------------------------

/// Magic byte identifying our ioctls (must match the userspace test).
const EDU_IOC_MAGIC: u32 = b'e' as u32;

/// `ioctl(fd, EDU_FACTORIAL, &v)`: `v` is a `u32` that goes in as `n` and comes back as `n!`.
/// `_IOWR` = the argument is both written by user space and read back by it.
const EDU_FACTORIAL: u32 = _IOWR::<u32>(EDU_IOC_MAGIC, 1);

/// 12! = 479001600 is the largest factorial that fits in the card's 32-bit result register.
const MAX_FACTORIAL_INPUT: u32 = 12;

/// How long an ioctl waits for the interrupt before giving up with `ETIMEDOUT`.
const WAIT_TIMEOUT_MS: u32 = 2000;

// --- State shared between process context and the interrupt handler --------------------------

/// Everything the ioctl path and the IRQ handler both need. One per bound card, behind an
/// `Arc` so the handler, the driver data and every open file can each hold a reference.
#[pin_data]
struct Shared {
    /// The register window.
    bar: Arc<Devres<Bar0>>,
    /// Serialises requests (the card computes one factorial at a time) and is the lock the
    /// `CondVar` needs. Only process context takes it: it is a `Mutex`, which may sleep.
    #[pin]
    request: Mutex<()>,
    /// Sleepers wait here until `finished` becomes 1.
    #[pin]
    done: CondVar,
    /// 0 = waiting for the card, 1 = `result` is valid. Written by the IRQ handler, so it is an
    /// atomic: the handler cannot take `request`.
    finished: Atomic<u32>,
    /// The card's answer, valid once `finished` is 1.
    result: Atomic<u32>,
}

// The one live `Shared`, or `None` when no card is bound. `MiscDevice::open` has no way to
// receive driver data, so it looks here. `global_lock!` makes a `static` lock; it must be
// initialised once before use (see `EduModule::init`).
kernel::sync::global_lock! {
    // SAFETY: initialised in `EduModule::init` before the driver is registered.
    unsafe(uninit) static CURRENT: Mutex<Option<Arc<Shared>>> = None;
}

impl Shared {
    /// Compute `n!` on the card and sleep until the interrupt says it is done.
    /// Runs in process context, so sleeping is allowed.
    fn factorial(&self, n: u32) -> Result<u32> {
        // One request at a time. After this line nobody else touches the card's FACTORIAL and
        // `finished`/`result`; the guard is also what `CondVar::wait` releases while sleeping.
        let mut guard = self.request.lock();

        self.finished.store(0, Relaxed);
        {
            // No `Bound` device here, so `try_access` (returns `None` after device removal).
            // The returned guard is an RCU read-side section: keep it short, never sleep with
            // it held. That is why this block ends before we wait.
            let bar = self.bar.try_access().ok_or(ENODEV)?;
            bar.write32(STATUS_IRQ_ON_FACTORIAL, Regs::STATUS);
            bar.write32(n, Regs::FACTORIAL); // starts the computation
        }

        let mut left = msecs_to_jiffies(WAIT_TIMEOUT_MS);
        // The loop re-checks the condition after every wake-up, because wake-ups may be
        // spurious. `Acquire` pairs with the handler's `Release` store of `finished`, so
        // we also see the `result` it stored before.
        while self.finished.load(ordering::Acquire) == 0 {
            // Atomically unlocks `request`, sleeps, and re-locks on wake-up.
            match self.done.wait_interruptible_timeout(&mut guard, left) {
                CondVarTimeoutResult::Woken { jiffies } => left = jiffies,
                // A signal (Ctrl-C) arrived. Tell the kernel to restart or fail the syscall.
                CondVarTimeoutResult::Signal { .. } => return Err(ERESTARTSYS),
                CondVarTimeoutResult::Timeout => return Err(ETIMEDOUT),
            }
        }
        Ok(self.result.load(Relaxed))
    }
}

// --- Interrupt handler -----------------------------------------------------------------------

/// The handler's state: just a reference to the shared data.
#[pin_data]
struct EduIrq {
    shared: Arc<Shared>,
}

impl irq::Handler for EduIrq {
    /// Hard IRQ context: fast, no sleeping, no `Mutex`. It acknowledges the card, publishes the
    /// result and wakes the sleeper.
    fn handle(&self, dev: &Device<Bound>) -> IrqReturn {
        let Ok(bar) = self.shared.bar.access(dev) else {
            return IrqReturn::None;
        };
        let reason = bar.read32(Regs::IRQ_STATUS);
        if reason == 0 {
            return IrqReturn::None; // shared line, not our card
        }
        bar.write32(reason, Regs::IRQ_ACK);

        if reason & IRQ_REASON_FACTORIAL_DONE != 0 {
            // Publish the result first, then the flag with `Release`, then wake up: a woken
            // task that sees `finished == 1` is guaranteed to see `result`.
            self.shared
                .result
                .store(bar.read32(Regs::FACTORIAL), Relaxed);
            self.shared.finished.store(1, ordering::Release);
            // Waking from IRQ context is fine: it only marks tasks runnable.
            self.shared.done.notify_all();
        }
        IrqReturn::Handled
    }
}

// --- /dev/edu --------------------------------------------------------------------------------

/// State of one open `/dev/edu` file.
#[pin_data]
struct EduFile {
    shared: Arc<Shared>,
    dev: ARef<Device>,
}

#[vtable]
impl MiscDevice for EduFile {
    type Ptr = Pin<KBox<Self>>;

    fn open(_file: &File, misc: &MiscDeviceRegistration<Self>) -> Result<Pin<KBox<Self>>> {
        // Fetch the shared state; `None` means the card was unbound meanwhile.
        let shared = CURRENT.lock().as_ref().ok_or(ENODEV)?.clone();
        KBox::pin_init(
            try_pin_init!(EduFile {
                shared,
                dev: ARef::from(misc.device()),
            }),
            GFP_KERNEL,
        )
    }

    fn ioctl(me: Pin<&EduFile>, _file: &File, cmd: u32, arg: usize) -> Result<isize> {
        if cmd != EDU_FACTORIAL {
            return Err(ENOTTY);
        }
        let arg = UserPtr::from_addr(arg);
        let size = _IOC_SIZE(cmd);

        let n = UserSlice::new(arg, size).reader().read::<u32>()?;
        if n > MAX_FACTORIAL_INPUT {
            return Err(EINVAL);
        }

        // This call sleeps until the interrupt arrives.
        let result = me.shared.factorial(n)?;
        dev_dbg!(me.dev, "{}! = {}\n", n, result);

        // Write the answer back over the argument.
        UserSlice::new(arg, size).writer().write(&result)?;
        Ok(0)
    }
}

// --- PCI driver ------------------------------------------------------------------------------

/// Driver data, one per bound card. Field order is drop order: first remove `/dev/edu`, then
/// free the IRQ, then release our references.
#[pin_data]
struct EduDriver {
    /// Owning this registration keeps `/dev/edu` in existence.
    #[pin]
    _misc: MiscDeviceRegistration<EduFile>,
    /// Owning this keeps the interrupt handler registered.
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

impl pci::Driver for EduDriver {
    type IdInfo = ();
    const ID_TABLE: pci::IdTable<Self::IdInfo> = &PCI_TABLE;

    fn probe(pdev: &pci::Device<Core>, _info: &Self::IdInfo) -> Result<Pin<KBox<Self>>> {
        if pdev.vendor_id().as_raw() != EDU_VENDOR_ID || pdev.device_id() != EDU_DEVICE_ID {
            return Err(ENODEV);
        }
        dev_info!(pdev.as_ref(), "edu_misc: probe\n");

        pdev.enable_device_mem()?;
        pdev.set_master();

        let bar = Arc::pin_init(
            pdev.iomap_region_sized::<{ Regs::END }>(EDU_REGS_BAR, c_str!("edu_misc")),
            GFP_KERNEL,
        )?;
        {
            let io = bar.access(pdev.as_ref())?;
            if io.read32(Regs::ID) & ID_SIGNATURE_MASK != ID_SIGNATURE {
                return Err(ENODEV);
            }
        }

        let shared = Arc::pin_init(
            try_pin_init!(Shared {
                bar,
                request <- new_mutex!(()),
                done <- new_condvar!(),
                finished: Atomic::new(0),
                result: Atomic::new(0),
            }),
            GFP_KERNEL,
        )?;

        // The initialiser closure takes ownership of what it captures: give it its own clone.
        let irq_shared = shared.clone();
        let irq = pdev.request_irq(
            EDU_IRQ_VECTOR,
            irq::Flags::SHARED,
            c_str!("edu_misc"),
            try_pin_init!(EduIrq { shared: irq_shared }),
        )?;

        // Publish the shared state *before* creating /dev/edu, so `open` always finds it.
        *CURRENT.lock() = Some(shared.clone());

        let drvdata = KBox::pin_init(
            try_pin_init!(Self {
                _misc <- MiscDeviceRegistration::register(MiscDeviceOptions {
                    name: c_str!("edu"),
                }),
                _irq <- irq,
                shared,
            }),
            GFP_KERNEL,
        );
        if drvdata.is_err() {
            *CURRENT.lock() = None; // do not leave a stale pointer if registration failed
        }
        drvdata
    }

    fn unbind(pdev: &pci::Device<Core>, this: Pin<&Self>) {
        // New opens must fail from now on; ioctls already running keep their `Arc`.
        *CURRENT.lock() = None;
        if let Ok(io) = this.shared.bar.access(pdev.as_ref()) {
            io.write32(STATUS_IRQ_DISABLED, Regs::STATUS);
        }
        dev_info!(pdev.as_ref(), "edu_misc: unbind\n");
    }
}

// --- Module ----------------------------------------------------------------------------------

/// `module_pci_driver!` would generate this for us; we write it out only to initialise the
/// `CURRENT` global lock before the driver can probe.
#[pin_data]
struct EduModule {
    #[pin]
    _driver: driver::Registration<pci::Adapter<EduDriver>>,
}

impl InPlaceModule for EduModule {
    fn init(module: &'static ThisModule) -> impl PinInit<Self, Error> {
        // SAFETY: called exactly once, before the PCI driver is registered (so before any use).
        unsafe { CURRENT.init() };
        try_pin_init!(Self {
            _driver <- driver::Registration::new(<Self as ModuleMetadata>::NAME, module),
        })
    }
}

module! {
    type: EduModule,
    name: "edu_misc",
    authors: ["edupsousa"],
    description: "Lesson 7: /dev/edu and sleeping on an interrupt",
    license: "GPL",
}
