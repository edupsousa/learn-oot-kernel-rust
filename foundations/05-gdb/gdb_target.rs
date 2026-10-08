// SPDX-License-Identifier: GPL-2.0

//! Lesson 5: a module made to be debugged with GDB.
//!
//! `/dev/gdbtarget` takes a decimal number, runs the Collatz sequence on it (halve if even,
//! else `3n+1`, until 1) and remembers how many steps it took. The logic is deliberately split
//! into small functions with distinct call paths so there is something to break on, step into
//! and watch:
//!
//! ```text
//!  write(fd, "27")
//!     └─ MiscDevice::write_iter        parse the number, lock the state
//!          └─ gdbt_run_collatz         loop, updates State
//!               └─ gdbt_collatz_step   one step of the sequence
//! ```
//!
//! Rust symbols are mangled (`_RNvXs...`), which makes `break` awkward. The helpers below use
//! `#[no_mangle]` + `#[inline(never)]` so `break gdbt_collatz_step` just works and the
//! compiler cannot fold them into their caller. Normal driver code would not do this.

use core::pin::Pin;

use kernel::{
    c_str,
    device::Device,
    fs::Kiocb,
    iov::{IovIterDest, IovIterSource},
    miscdevice::{MiscDevice, MiscDeviceOptions, MiscDeviceRegistration},
    new_mutex,
    prelude::*,
    str::CString,
    sync::{aref::ARef, Mutex},
};

module! {
    type: GdbTargetModule,
    name: "gdb_target",
    authors: ["edupsousa"],
    description: "Lesson 5: a module to debug with GDB",
    license: "GPL",
}

#[pin_data]
struct GdbTargetModule {
    #[pin]
    _miscdev: MiscDeviceRegistration<GdbTarget>,
}

impl kernel::InPlaceModule for GdbTargetModule {
    fn init(_module: &'static ThisModule) -> impl PinInit<Self, Error> {
        pr_info!("gdb_target: init\n");
        let options = MiscDeviceOptions {
            name: c_str!("gdbtarget"),
        };
        try_pin_init!(Self {
            _miscdev <- MiscDeviceRegistration::register(options),
        })
    }
}

/// The state worth inspecting from GDB: `print *state`, or `watch state.calls`.
struct State {
    /// How many numbers this open file has processed.
    calls: u32,
    /// The last number written.
    last_input: u64,
    /// Steps the last sequence took.
    last_steps: u32,
    /// Highest value the last sequence reached.
    last_peak: u64,
}

#[pin_data(PinnedDrop)]
struct GdbTarget {
    #[pin]
    state: Mutex<State>,
    dev: ARef<Device>,
}

// ---------------------------------------------------------------------------------------------
// Functions to put breakpoints on
// ---------------------------------------------------------------------------------------------

/// One Collatz step. Called once per step, so a *conditional* breakpoint is the way to stop
/// on an interesting value: `break gdbt_collatz_step if n == 9232`.
#[no_mangle]
#[inline(never)]
pub extern "C" fn gdbt_collatz_step(n: u64) -> u64 {
    if n % 2 == 0 {
        n / 2
    } else {
        3 * n + 1
    }
}

/// Runs the whole sequence for `n`, recording the result in `state`.
///
/// `state` is the data behind the mutex guard, so `watch -l state.last_steps` triggers on
/// every update. Returns the number of steps.
#[no_mangle]
#[inline(never)]
fn gdbt_run_collatz(state: &mut State, n: u64) -> u32 {
    state.calls += 1;
    state.last_input = n;
    state.last_steps = 0;
    state.last_peak = n;

    let mut cur = n;
    while cur > 1 {
        cur = gdbt_collatz_step(cur);
        state.last_steps += 1;
        if cur > state.last_peak {
            state.last_peak = cur;
        }
    }
    state.last_steps
}

/// Parses ASCII decimal digits (ignoring a trailing newline). Returns `EINVAL` for anything
/// else, so `echo abc > /dev/gdbtarget` shows an error path to step through.
#[no_mangle]
#[inline(never)]
fn gdbt_parse(buf: &[u8]) -> Result<u64> {
    let digits = buf.strip_suffix(b"\n").unwrap_or(buf);
    if digits.is_empty() {
        return Err(EINVAL);
    }
    let mut n: u64 = 0;
    for &b in digits {
        if !b.is_ascii_digit() {
            return Err(EINVAL);
        }
        n = n
            .checked_mul(10)
            .and_then(|v| v.checked_add((b - b'0') as u64))
            .ok_or(ERANGE)?;
    }
    Ok(n)
}

#[vtable]
impl MiscDevice for GdbTarget {
    type Ptr = Pin<KBox<Self>>;

    fn open(_file: &kernel::fs::File, misc: &MiscDeviceRegistration<Self>) -> Result<Pin<KBox<Self>>> {
        let dev = ARef::from(misc.device());
        dev_info!(dev, "open\n");
        KBox::try_pin_init(
            try_pin_init! {
                GdbTarget {
                    state <- new_mutex!(State {
                        calls: 0,
                        last_input: 0,
                        last_steps: 0,
                        last_peak: 0,
                    }),
                    dev: dev,
                }
            },
            GFP_KERNEL,
        )
    }

    /// Writes a number, runs the sequence.
    fn write_iter(mut kiocb: Kiocb<'_, Self::Ptr>, iov: &mut IovIterSource<'_>) -> Result<usize> {
        let me = kiocb.file();

        let mut buf = KVVec::new();
        let len = iov.copy_from_iter_vec(&mut buf, GFP_KERNEL)?;
        let n = gdbt_parse(&buf)?;

        let mut state = me.state.lock();
        let steps = gdbt_run_collatz(&mut state, n);
        dev_info!(me.dev, "collatz({}) = {} steps\n", n, steps);

        // Rewind so a following `read()` on the same descriptor sees the new result.
        *kiocb.ki_pos_mut() = 0;
        Ok(len)
    }

    /// Reads back a one-line summary of the last result.
    fn read_iter(mut kiocb: Kiocb<'_, Self::Ptr>, iov: &mut IovIterDest<'_>) -> Result<usize> {
        let me = kiocb.file();
        let text = {
            let s = me.state.lock();
            CString::try_from_fmt(fmt!(
                "calls={} n={} steps={} peak={}\n",
                s.calls,
                s.last_input,
                s.last_steps,
                s.last_peak
            ))?
        };
        iov.simple_read_from_buffer(kiocb.ki_pos_mut(), text.to_bytes())
    }
}

#[pinned_drop]
impl PinnedDrop for GdbTarget {
    fn drop(self: Pin<&mut Self>) {
        dev_info!(self.dev, "release\n");
    }
}
