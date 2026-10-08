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
//!
//! # Why break things on purpose?
//!
//! Kernel bugs are hard to find because they often stay silent: a bad memory read usually
//! "works" and returns garbage, and a lock mistake only hurts when two CPUs hit it at the
//! same instant. So the kernel can be built with **debugging detectors** that watch for these
//! mistakes and print a report. Learning to read those reports is a core skill, and the
//! easiest way to learn is to cause each bug in a controlled setting. The detectors are:
//!
//! - **lockdep** (`PROVE_LOCKING`): records the order in which locks are taken and warns when
//!   the order could cause a *deadlock* (two threads each waiting forever for a lock the other
//!   holds), even if the deadlock did not actually happen this time.
//! - **`DEBUG_ATOMIC_SLEEP`**: warns when code *sleeps* (lets the CPU run something else while
//!   it waits) in a place where that is forbidden, such as while holding a spinlock.
//! - **KASAN** (Kernel Address SANitizer): marks the memory around and after every allocation
//!   as off-limits and reports any access to it. It catches reading past the end of a buffer
//!   (*out-of-bounds*) and using memory after it was freed (*use-after-free*).
//!
//! # How the lesson triggers the bugs
//!
//! Like lesson 3, the module registers a misc device (`/dev/broken`). Writing a word to it
//! selects the bug to run. We use `write` as a tiny command line, so no `ioctl` is needed.
//! Safe Rust prevents the memory bugs, which is why those two use `unsafe` blocks: `unsafe`
//! tells the compiler "I take responsibility for this code", and here we deliberately don't.

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

/// The module while loaded. As in lesson 3, it only owns the device registration: `/dev/broken`
/// exists while this value exists, and `rmmod` drops it, which removes the device.
#[pin_data]
struct BrokenModule {
    #[pin]
    _miscdev: MiscDeviceRegistration<Broken>,
}

// Same pattern as lesson 3: build the module in place, registering the device on the way.
// If registration fails the error propagates and `insmod` fails.
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
///
/// The `()` in `Mutex<()>` means the locks protect no data; we only care about the act of
/// locking. Two kinds of lock appear:
/// - `Mutex`: a thread that finds it taken is put to *sleep* until it is free. Sleeping is
///   only allowed in "process context" (code running on behalf of a program, like our `write`).
/// - `SpinLock`: a thread that finds it taken *busy-waits* (spins) on the CPU. It is meant for
///   very short critical sections and for code that must not sleep (e.g. interrupt handlers).
///   While you hold one, you must not do anything that could sleep.
#[pin_data]
struct Broken {
    #[pin]
    a: Mutex<()>,
    #[pin]
    b: Mutex<()>,
    #[pin]
    spin: SpinLock<()>,
    // Handle to the kernel device, used by `dev_info!`/`dev_err!` to label log lines.
    dev: ARef<Device>,
}

// As in lesson 3, `#[vtable]` lists which `MiscDevice` operations this driver implements.
#[vtable]
impl MiscDevice for Broken {
    // What `open` returns: a pinned, heap-allocated `Broken`, one per `open()` of the device.
    type Ptr = Pin<KBox<Self>>;

    // Called on every `open("/dev/broken")`: allocate the per-open state and create the locks.
    // `GFP_KERNEL` = "normal allocation, may sleep to get memory"; failure becomes `ENOMEM`.
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

    // Reading is not supported: we only use `write` as a command channel. Returning 0 means
    // end-of-file, so `cat /dev/broken` just prints nothing.
    fn read_iter(_kiocb: Kiocb<'_, Self::Ptr>, _iov: &mut IovIterDest<'_>) -> Result<usize> {
        Ok(0)
    }

    fn write_iter(kiocb: Kiocb<'_, Self::Ptr>, iov: &mut IovIterSource<'_>) -> Result<usize> {
        // `me` is the `Broken` created by `open` for this file descriptor.
        let me = kiocb.file();

        // Copy the bytes the user wrote (e.g. "abba\n") from user memory into a kernel vector.
        // The kernel never reads user pointers directly; `iov` does checked copies and returns
        // an error (`EFAULT`) instead of crashing if the user passed a bad address.
        let mut cmd = KVVec::new();
        let len = iov.copy_from_iter_vec(&mut cmd, GFP_KERNEL)?;

        // Ignore the trailing newline that `echo` adds.
        let cmd = cmd.strip_suffix(b"\n").unwrap_or(&cmd);

        // Compare the bytes against each known command and run the matching buggy function.
        // `b"abba"` is a byte-string literal (`&[u8; 4]`), not text, because the data is bytes.
        match cmd {
            b"abba" => me.abba(),
            b"sleep" => me.sleep_in_atomic(),
            b"oob" => me.out_of_bounds(),
            b"uaf" => me.use_after_free(),
            _ => {
                dev_err!(me.dev, "unknown command\n");
                // `EINVAL` = "invalid argument"; the shell's `echo` then reports a write error.
                return Err(EINVAL);
            }
        }
        // Report that we consumed all the bytes, otherwise the caller would retry the write.
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
            // `lock()` returns a guard that unlocks when dropped. Locals are dropped in reverse
            // order of creation, so at the closing brace `_b` is released first, then `_a`.
            // The underscore prefix means "we hold the guard only for its effect".
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
        // Take the spinlock: from here until the end of the function we are in "atomic context".
        let _s = self.spin.lock();
        // Taking a mutex may sleep, which is forbidden while the spinlock is held. The check
        // fires as soon as we call `lock()`, even though nobody else holds `a` and so we would
        // not really sleep this time: the kernel flags code that *could* sleep.
        let _a = self.a.lock();
    }

    /// Bug 3: out-of-bounds read, only possible with `unsafe`.
    ///
    /// We own a `BUF_LEN`-byte allocation and read the byte just past its end through a raw pointer.
    /// Safe Rust would never let us (indexing is bounds-checked), `unsafe` removes that check
    /// and KASAN, which poisons the bytes around every allocation, catches it at run time.
    fn out_of_bounds(&self) {
        dev_info!(self.dev, "oob: reading one byte past the end of a buffer of {} bytes\n", BUF_LEN);
        // `KBox` is the kernel's `Box`: one heap allocation, here an array of zeroed bytes.
        // Allocation can fail, so `new` returns a `Result`; `if let Ok` skips the bug if it did.
        if let Ok(buf) = KBox::new([0u8; BUF_LEN], GFP_KERNEL) {
            // A raw pointer is a plain address with no bounds information.
            let p = buf.as_ptr();
            // `unsafe` is required to dereference a raw pointer. The `SAFETY:` comment is where
            // real code explains why the access is valid; here it admits that it is not.
            // `read_volatile` stops the optimiser from deleting the "unused" read.
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
            // `into_raw` gives up the box's ownership and returns the bare pointer; Rust will
            // no longer free the memory for us.
            let p = KBox::into_raw(b);
            // Rebuild the box and drop it immediately: the memory goes back to the allocator.
            // SAFETY: `p` came from `into_raw`, so rebuilding the box is valid; this frees it.
            drop(unsafe { KBox::from_raw(p) });
            // `p` still holds the old address, but the memory is no longer ours (a "dangling"
            // pointer). Without KASAN this read would likely succeed and look fine, which is
            // exactly what makes use-after-free bugs dangerous.
            // SAFETY: deliberately NOT safe, `p` is dangling now.
            let v = unsafe { core::ptr::read_volatile(p) };
            dev_info!(self.dev, "uaf: read {:#x}\n", v);
        }
    }
}
