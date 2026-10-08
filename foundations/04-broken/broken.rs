// SPDX-License-Identifier: GPL-2.0

//! Lesson 4: break things on purpose to read lockdep, `DEBUG_ATOMIC_SLEEP` and KASAN reports.
//!
//! The module creates `/dev/broken`. Writing a command to it triggers one bug:
//!
//! ```text
//! echo abba  > /dev/broken   # lockdep: lock-order inversion (possible deadlock)
//! echo sleep > /dev/broken   # "sleeping function called from invalid context"
//! echo oob   > /dev/broken   # KASAN: out-of-bounds read   (needs `unsafe`)
//! echo uaf   > /dev/broken   # KASAN: use-after-free       (needs `unsafe`)
//! ```
//!
//! Then read the report with `dmesg`. None of these bugs crash the machine by themselves,
//! the kernel detects and reports them. Only run this module in the VM.

use core::pin::Pin;

use kernel::{
    c_str,
    device::Device,
    fs::{File, Kiocb},
    iov::{IovIterDest, IovIterSource},
    miscdevice::{MiscDevice, MiscDeviceOptions, MiscDeviceRegistration},
    new_mutex, new_spinlock,
    prelude::*,
    sync::{aref::ARef, Mutex, SpinLock},
};

module! {
    type: BrokenModule,
    name: "broken",
    authors: ["edupsousa"],
    description: "Lesson 4: deliberately buggy module",
    license: "GPL",
}

/// Size in bytes of the buffer used by the `oob` command. The bug is reading at index
/// `BUF_LEN`, the first byte past the end, so this one number defines both.
const BUF_LEN: usize = 8;

#[pin_data]
struct BrokenModule {
    #[pin]
    _miscdev: MiscDeviceRegistration<Broken>,
}

impl kernel::InPlaceModule for BrokenModule {
    fn init(_module: &'static ThisModule) -> impl PinInit<Self, Error> {
        pr_info!("broken: init, try: echo abba|sleep|oob|uaf > /dev/broken\n");
        try_pin_init!(Self {
            _miscdev <- MiscDeviceRegistration::register(MiscDeviceOptions {
                name: c_str!("broken"),
            }),
        })
    }
}

/// Per-open state: two mutexes (for the ABBA bug) and a spinlock (for the sleep bug).
///
/// Lockdep tracks lock *classes*: every `new_mutex!`/`new_spinlock!` call site is one class, so
/// `a` and `b` below are two different classes, which is what lets lockdep reason about order.
#[pin_data]
struct Broken {
    #[pin]
    a: Mutex<()>,
    #[pin]
    b: Mutex<()>,
    #[pin]
    spin: SpinLock<()>,
    dev: ARef<Device>,
}

#[vtable]
impl MiscDevice for Broken {
    type Ptr = Pin<KBox<Self>>;

    fn open(_file: &File, misc: &MiscDeviceRegistration<Self>) -> Result<Pin<KBox<Self>>> {
        KBox::try_pin_init(
            try_pin_init! {
                Broken {
                    a <- new_mutex!(()),
                    b <- new_mutex!(()),
                    spin <- new_spinlock!(()),
                    dev: ARef::from(misc.device()),
                }
            },
            GFP_KERNEL,
        )
    }

    // Reading is not supported: we only use `write` as a command channel.
    fn read_iter(_kiocb: Kiocb<'_, Self::Ptr>, _iov: &mut IovIterDest<'_>) -> Result<usize> {
        Ok(0)
    }

    fn write_iter(kiocb: Kiocb<'_, Self::Ptr>, iov: &mut IovIterSource<'_>) -> Result<usize> {
        let me = kiocb.file();

        let mut cmd = KVVec::new();
        let len = iov.copy_from_iter_vec(&mut cmd, GFP_KERNEL)?;

        // Ignore the trailing newline that `echo` adds.
        let cmd = cmd.strip_suffix(b"\n").unwrap_or(&cmd);

        match cmd {
            b"abba" => me.abba(),
            b"sleep" => me.sleep_in_atomic(),
            b"oob" => me.out_of_bounds(),
            b"uaf" => me.use_after_free(),
            _ => {
                dev_err!(me.dev, "unknown command\n");
                return Err(EINVAL);
            }
        }
        Ok(len)
    }
}

impl Broken {
    /// Bug 1: lock-order inversion.
    ///
    /// Path 1 takes A then B; path 2 takes B then A. Run one after the other by a single thread
    /// nothing deadlocks, but if two threads ran them concurrently each could hold one lock and
    /// wait forever for the other. Lockdep remembers "A was held while taking B", sees B then A
    /// later, finds the cycle and prints "possible circular locking dependency detected".
    fn abba(&self) {
        dev_info!(self.dev, "abba: A then B\n");
        {
            let _a = self.a.lock();
            let _b = self.b.lock();
        }
        dev_info!(self.dev, "abba: B then A (lockdep should complain now)\n");
        {
            let _b = self.b.lock();
            let _a = self.a.lock();
        }
    }

    /// Bug 2: sleeping while holding a spinlock.
    ///
    /// A spinlock holder runs with preemption disabled and must not sleep. `Mutex::lock` may
    /// sleep, so calling it here triggers "BUG: sleeping function called from invalid context"
    /// (`DEBUG_ATOMIC_SLEEP`). The Rust type system does not catch this: both guards are fine
    /// types, the problem is the *context*.
    fn sleep_in_atomic(&self) {
        dev_info!(self.dev, "sleep: mutex lock under a spinlock\n");
        let _s = self.spin.lock();
        let _a = self.a.lock();
    }

    /// Bug 3: out-of-bounds read, only possible with `unsafe`.
    ///
    /// We own a `BUF_LEN`-byte allocation and read the byte just past its end through a raw pointer.
    /// Safe Rust would never let us (indexing is bounds-checked), `unsafe` removes that check
    /// and KASAN, which poisons the bytes around every allocation, catches it at run time.
    fn out_of_bounds(&self) {
        dev_info!(self.dev, "oob: reading one byte past the end of a buffer of {} bytes\n", BUF_LEN);
        if let Ok(buf) = KBox::new([0u8; BUF_LEN], GFP_KERNEL) {
            let p = buf.as_ptr();
            // SAFETY: deliberately NOT safe, `p.add(BUF_LEN)` is one past the end of the allocation.
            let v = unsafe { core::ptr::read_volatile(p.add(BUF_LEN)) };
            dev_info!(self.dev, "oob: read {}\n", v);
        }
    }

    /// Bug 4: use after free, only possible with `unsafe`.
    ///
    /// We turn a `KBox` into a raw pointer, free the memory by rebuilding and dropping the box,
    /// then read through the stale pointer. KASAN poisons freed memory, so the read is reported
    /// along with the stacks of where it was allocated and freed.
    fn use_after_free(&self) {
        dev_info!(self.dev, "uaf: reading freed memory\n");
        if let Ok(b) = KBox::new(0x1234_5678_u64, GFP_KERNEL) {
            let p = KBox::into_raw(b);
            // SAFETY: `p` came from `into_raw`, so rebuilding the box is valid; this frees it.
            drop(unsafe { KBox::from_raw(p) });
            // SAFETY: deliberately NOT safe, `p` is dangling now.
            let v = unsafe { core::ptr::read_volatile(p) };
            dev_info!(self.dev, "uaf: read {:#x}\n", v);
        }
    }
}
