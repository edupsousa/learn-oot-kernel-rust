// SPDX-License-Identifier: GPL-2.0

//! Lesson 3: `/dev/hello`, a misc character device with `read`, `write` and `ioctl`.
//!
//! Big picture of what happens when a program uses the device:
//!
//! ```text
//!  userspace                         kernel (this file)
//!  ---------                         ------------------
//!  open("/dev/hello")      ──────▶   MiscDevice::open      (creates one HelloDev per open)
//!  write(fd, buf, n)       ──────▶   MiscDevice::write_iter
//!  read(fd, buf, n)        ──────▶   MiscDevice::read_iter
//!  ioctl(fd, cmd, arg)     ──────▶   MiscDevice::ioctl
//!  close(fd)               ──────▶   HelloDev is dropped   (PinnedDrop::drop)
//! ```
//!
//! A *misc device* is the simplest kind of character device: the kernel's "misc" subsystem
//! hands out the device number for us and `devtmpfs` creates the `/dev/<name>` node
//! automatically, so we do not need `mknod`, class or major/minor handling.

use core::pin::Pin;

use kernel::{
    c_str,
    device::Device,
    fs::{File, Kiocb},
    ioctl::{_IO, _IOC_SIZE, _IOR, _IOW},
    iov::{IovIterDest, IovIterSource},
    miscdevice::{MiscDevice, MiscDeviceOptions, MiscDeviceRegistration},
    new_mutex,
    prelude::*,
    sync::{aref::ARef, Mutex},
    uaccess::{UserSlice, UserSliceReader, UserSliceWriter},
};

// ---------------------------------------------------------------------------------------------
// ioctl command numbers
// ---------------------------------------------------------------------------------------------
//
// An ioctl command is a single `u32` that packs four fields:
//
//   | direction (2 bits) | argument size (14 bits) | "magic" type (8 bits) | number (8 bits) |
//
// - direction: does the user pass data in (write), get data out (read), both, or neither?
// - size:      how many bytes the argument occupies (here `size_of::<i32>()` = 4).
// - magic:     one character that identifies our driver, so numbers do not clash with others.
//              We use 'h' for "hello".
// - number:    our own command index.
//
// `_IO`   = no argument.
// `_IOR`  = the user *reads* a value from the kernel (kernel writes to user memory).
// `_IOW`  = the user *writes* a value to the kernel (kernel reads from user memory).
// The direction is always from the user's point of view, which is easy to get backwards.
//
// The userspace program (`ioctl_test.c`) must build the same numbers with the C macros, otherwise
// the `match` in `ioctl` below will not recognise them.

/// The "magic" type byte shared by all our ioctls: 'h' for "hello". It must match `ioctl_test.c`.
const HELLO_IOC_MAGIC: u32 = 'h' as u32;

/// ioctl number 1: no argument, makes the driver log a greeting.
const HELLO_SAY: u32 = _IO(HELLO_IOC_MAGIC, 1);
/// ioctl number 2: the driver writes the stored `i32` to the user's pointer.
const HELLO_GET_VALUE: u32 = _IOR::<i32>(HELLO_IOC_MAGIC, 2);
/// ioctl number 3: the driver reads an `i32` from the user's pointer and stores it.
const HELLO_SET_VALUE: u32 = _IOW::<i32>(HELLO_IOC_MAGIC, 3);

// `module!` generates the C-visible module metadata and the `init_module`/`cleanup_module` entry
// points. `type:` names the struct below that represents the loaded module.
module! {
    type: HelloDevModule,
    name: "hello_dev",
    authors: ["edupsousa"],
    description: "Lesson 3: /dev/hello misc device",
    license: "GPL",
}

// ---------------------------------------------------------------------------------------------
// The module itself
// ---------------------------------------------------------------------------------------------

/// The value that lives as long as the module is loaded.
///
/// It owns the device *registration*. This is RAII applied to a kernel resource: while this
/// struct exists `/dev/hello` exists, and when the module is removed (`rmmod`) the struct is
/// dropped, which unregisters the device. There is no explicit "exit" function to write, and
/// no way to forget to unregister.
///
/// `#[pin_data]` / `#[pin]` are needed (as in lesson 2) because the registration embeds
/// kernel structures that must never move in memory once registered.
#[pin_data]
struct HelloDevModule {
    // The leading underscore tells the compiler the field is intentionally never read: we keep
    // it only for its side effect of staying registered until drop.
    #[pin]
    _miscdev: MiscDeviceRegistration<HelloDev>,
}

// `InPlaceModule` (not `Module`) because we build a pinned value in place, see lesson 2.
impl kernel::InPlaceModule for HelloDevModule {
    fn init(_module: &'static ThisModule) -> impl PinInit<Self, Error> {
        pr_info!("hello_dev: init\n");

        // The `name` becomes the node `/dev/hello`.
        let options = MiscDeviceOptions {
            name: c_str!("hello"),
        };

        // `<-` means "initialise this pinned field in place with this initialiser".
        // `register` asks the misc subsystem to create the device; if it fails, the error is
        // propagated and `insmod` fails.
        try_pin_init!(Self {
            _miscdev <- MiscDeviceRegistration::register(options),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Per-open state
// ---------------------------------------------------------------------------------------------

/// The data protected by the lock: a number and a byte buffer.
///
/// `KVVec` is a growable array that uses `kmalloc` for small sizes and falls back to `vmalloc`
/// for large ones, so a user can write a big buffer without needing a huge contiguous chunk
/// of physical memory.
struct Inner {
    value: i32,
    buffer: KVVec<u8>,
}

/// The state of one open file.
///
/// Every `open("/dev/hello")` creates a new `HelloDev`, so two descriptors do not share their
/// buffer or value. (A driver that wants shared state would instead keep it in the module
/// struct and hand out references to it.) Sharing the same descriptor, e.g. through `dup()`
/// or `fork()`, *does* share it, and several threads may call in at once. That is why the data
/// sits behind a `Mutex`.
#[pin_data(PinnedDrop)]
struct HelloDev {
    // Same lock-owns-the-data pattern as lesson 2: `inner` can only be reached via `lock()`.
    #[pin]
    inner: Mutex<Inner>,
    // A reference-counted handle to the kernel `struct device` of `/dev/hello`. `dev_info!`
    // and friends use it to prefix log lines with the device name. `ARef` keeps the device
    // alive for as long as we hold it.
    dev: ARef<Device>,
}

// `MiscDevice` is a trait with many optional methods (open, read_iter, ioctl, mmap, ...).
// `#[vtable]` records which ones we implement, so the generated C `file_operations` table
// contains a function pointer only for those, and the kernel uses its defaults for the rest.
#[vtable]
impl MiscDevice for HelloDev {
    /// What `open` returns is stored in the kernel's `struct file` (its `private_data`) and
    /// handed back to every later call on that descriptor. A pinned, heap-allocated `HelloDev`
    /// is exactly what we want: it is freed (and `drop` runs) when the file is released.
    type Ptr = Pin<KBox<Self>>;

    /// Called on every `open()` of `/dev/hello`.
    ///
    /// `_file` is the new kernel file object (unused here). `misc` is our registration; we use
    /// it to get at the underlying `struct device`.
    fn open(_file: &File, misc: &MiscDeviceRegistration<Self>) -> Result<Pin<KBox<Self>>> {
        // Take our own counted reference to the device so it outlives this call.
        let dev = ARef::from(misc.device());
        dev_info!(dev, "open\n");

        // Allocate the `HelloDev` on the heap and initialise it in place. This allocation can
        // fail (`GFP_KERNEL` = "may sleep to get memory", fine in process context), in which
        // case `open()` returns `-ENOMEM` to the user instead of crashing.
        KBox::try_pin_init(
            try_pin_init! {
                HelloDev {
                    inner <- new_mutex!(Inner {
                        value: 0,
                        buffer: KVVec::new(),
                    }),
                    dev: dev,
                }
            },
            GFP_KERNEL,
        )
    }

    /// Called for `read()`: copy bytes from the device to the user's buffer.
    ///
    /// - `kiocb` describes this I/O request. `kiocb.file()` gives back our `HelloDev` (the
    ///   value returned by `open`), and `ki_pos_mut()` is the file offset.
    /// - `iov` is the destination: a safe wrapper around the user's buffer(s). We never see a
    ///   raw user pointer.
    ///
    /// Returns the number of bytes copied; `0` means end-of-file.
    fn read_iter(mut kiocb: Kiocb<'_, Self::Ptr>, iov: &mut IovIterDest<'_>) -> Result<usize> {
        let me = kiocb.file();
        dev_info!(me.dev, "read\n");

        // Hold the lock while we look at the buffer. The guard unlocks when it goes out of
        // scope at the end of the function.
        let inner = me.inner.lock();

        // Copies from our kernel buffer to user memory, starting at the current file offset,
        // and advances the offset by the amount copied. So a second `read()` continues where
        // the first stopped, and eventually returns 0 (EOF). If the user's buffer is invalid
        // this returns `EFAULT` for us.
        iov.simple_read_from_buffer(kiocb.ki_pos_mut(), &inner.buffer)
    }

    /// Called for `write()`: copy bytes from the user's buffer into the device.
    ///
    /// Here `iov` is the *source* of the data (user memory).
    fn write_iter(mut kiocb: Kiocb<'_, Self::Ptr>, iov: &mut IovIterSource<'_>) -> Result<usize> {
        let me = kiocb.file();
        dev_info!(me.dev, "write\n");

        // `mut` because we modify the protected data.
        let mut inner = me.inner.lock();

        // Each write replaces the previous contents (a simplification for the lesson).
        inner.buffer.clear();

        // Copies all the user's bytes into our vector, growing it as needed. Growing can fail
        // with `ENOMEM` and the copy can fail with `EFAULT`; `?` returns either to the user.
        let len = iov.copy_from_iter_vec(&mut inner.buffer, GFP_KERNEL)?;

        // Rewind the file offset so the next `read()` starts at the beginning of the new data.
        *kiocb.ki_pos_mut() = 0;

        // Tell the caller how many bytes we consumed (what `write()` returns).
        Ok(len)
    }

    /// Called for `ioctl()`: device-specific commands that do not fit read/write.
    ///
    /// - `me` is our `HelloDev`, borrowed (and pinned) for the duration of the call.
    /// - `cmd` is the packed command number built with `_IO*` above.
    /// - `arg` is the raw third argument from userspace. For commands that carry data it is
    ///   a **user-space address**.
    fn ioctl(me: Pin<&HelloDev>, _file: &File, cmd: u32, arg: usize) -> Result<isize> {
        // `arg` is untrusted. It could be null, point into kernel memory, or be unmapped. We
        // wrap it in `UserPtr` to mark it as "an address in user space, never dereference
        // directly". The only way to touch it is a `UserSlice`, which performs checked copies
        // and reports `EFAULT` instead of crashing.
        let arg = UserPtr::from_addr(arg);

        // The size of the argument is encoded in the command number itself.
        let size = _IOC_SIZE(cmd);

        match cmd {
            HELLO_SAY => dev_info!(me.dev, "hello from ioctl\n"),
            // A `writer` copies data *to* user space (the user is reading from us).
            HELLO_GET_VALUE => me.get_value(UserSlice::new(arg, size).writer())?,
            // A `reader` copies data *from* user space (the user is writing to us).
            HELLO_SET_VALUE => me.set_value(UserSlice::new(arg, size).reader())?,
            _ => {
                dev_err!(me.dev, "unknown ioctl {:#x}\n", cmd);
                // By convention `ENOTTY` ("not a typewriter", for historical reasons) means
                // "this device does not support that ioctl".
                return Err(ENOTTY);
            }
        }

        // ioctl returns 0 on success; a negative errno on failure comes from `Err` above.
        Ok(0)
    }
}

// `PinnedDrop` is the `Drop` for pinned types. The kernel calls this when the last reference
// to the open file goes away, i.e. after `close()` (and any `dup`ed copies are closed too).
// The `Mutex`, the buffer and the device reference are then freed automatically by Rust.
#[pinned_drop]
impl PinnedDrop for HelloDev {
    fn drop(self: Pin<&mut Self>) {
        dev_info!(self.dev, "release\n");
    }
}

// Helpers that implement the two data-carrying ioctls. Splitting them out keeps `ioctl` short.
impl HelloDev {
    /// `HELLO_SET_VALUE`: read an `i32` from user space and store it.
    fn set_value(&self, mut reader: UserSliceReader) -> Result {
        // Copies 4 bytes from user memory. Returns `EFAULT` if the address is bad. The
        // `i32` type is checked to be plain data that is safe to build from arbitrary bytes.
        let new_value = reader.read::<i32>()?;

        // Lock only for the store. The temporary guard is dropped at the end of the statement.
        self.inner.lock().value = new_value;
        dev_info!(self.dev, "value set to {}\n", new_value);
        Ok(())
    }

    /// `HELLO_GET_VALUE`: copy the stored `i32` to user space.
    fn get_value(&self, mut writer: UserSliceWriter) -> Result {
        // Copy the value out and let the guard drop immediately (end of the statement), so the
        // lock is NOT held during the user copy below. Touching user memory can page-fault,
        // which may sleep while the kernel reads the page in from disk; sleeping with a lock
        // held would block every other caller of this file, and in other contexts is a bug.
        let value = self.inner.lock().value;

        // Copies 4 bytes to user memory; `EFAULT` if the address is bad.
        writer.write::<i32>(&value)?;
        Ok(())
    }
}
