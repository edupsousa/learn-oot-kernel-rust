// SPDX-License-Identifier: GPL-2.0

//! Lesson 2: module state, pinned `Mutex` and fallible allocation.
//!
//! Lesson 1 had a module with no data. Real modules keep state (counters, buffers, device
//! handles) and that raises three questions, each answered by a section of this lesson:
//!
//! 1. **Who may touch the data, and when?** The kernel runs on many CPU cores at once and
//!    other code (or other processes) may call into your module simultaneously. Two cores
//!    changing the same variable at the same time is a *data race*. The cure is a **lock**:
//!    only the code that holds the lock may touch the data. Rust's kernel `Mutex<T>` makes
//!    this impossible to get wrong, because the data lives *inside* the lock and the only way
//!    to reach it is to call `lock()`.
//! 2. **Where does the data live?** Some kernel structures (a `Mutex` among them) contain
//!    pointers to themselves, so they must not be moved to another address after creation.
//!    Rust calls this being *pinned*. The kernel's `pin-init` machinery builds such values
//!    directly at their final location.
//! 3. **What if memory runs out?** User programs rarely worry about `malloc` failing. In the
//!    kernel it can happen, so allocations return a `Result` that you must handle.
//!
//! What the module does: at load it bumps a counter three times and records each step in a
//! list; at unload it prints both. Expected output of `rmmod counter`:
//! `counter: exit, ticks=3 history=[1, 4, 9]`.

// New imports compared to lesson 1:
//   - `new_mutex!` creates a kernel mutex (a lock that puts the caller to sleep while waiting);
//   - `Mutex` is the lock type itself.
use kernel::{new_mutex, prelude::*, sync::Mutex};

module! {
    type: Counter,
    name: "counter",
    authors: ["edupsousa"],
    description: "Lesson 2: module state",
    license: "GPL",
}

/// How many times `init` bumps the counter. The history then holds `[1, 4, 9]`
/// (`ticks * i` for i = 1..=3), the value `rmmod` prints.
const INIT_TICKS: u32 = 3;

/// The data the lock protects.
///
/// Nobody can read or write these fields without first taking the lock (see `Counter`).
struct State {
    ticks: u32,
    // `KVec<T>` is the kernel's version of `Vec<T>`, a growable array. The difference is that
    // growing it can fail (out of memory), so its methods take an allocation flag and return
    // a `Result` instead of aborting.
    history: KVec<u32>,
}

// The module's state, as lesson 1's `Hello` but now with a field.
//
// `#[pin_data]` says "this struct will be *pinned*": once created it never moves in memory.
// Why it matters: a kernel `Mutex` contains a linked list of the tasks waiting for it, and
// those list entries point back to the mutex. Moving the mutex would leave them pointing at
// the old, now invalid, address. Fields marked `#[pin]` are the ones that need this guarantee
// and are initialised *in place*. `PinnedDrop` in the attribute means we write a custom
// cleanup (below) with the pinned variant of `Drop`.
#[pin_data(PinnedDrop)]
struct Counter {
    // `Mutex<State>`: the lock *owns* the `State`. There is no way to reach `ticks` or
    // `history` except through `state.lock()`, so forgetting to lock is a compile error, not a
    // rare crash on a busy machine.
    #[pin]
    state: Mutex<State>,
}

// `InPlaceModule` replaces lesson 1's `Module`. With `Module`, `init` returns a finished value
// that Rust then moves into place, which pinned types cannot allow. With `InPlaceModule`,
// `init` instead returns an *initialiser*: a recipe describing how to build the struct, which
// the kernel runs directly at the struct's final address.
impl kernel::InPlaceModule for Counter {
    fn init(_module: &'static ThisModule) -> impl PinInit<Self, Error> {
        pr_info!("counter: init\n");

        // `try_pin_init!` writes the recipe. "try" = it may fail, and if it does, the load
        // fails cleanly and anything already built is torn down.
        try_pin_init!(Self {
            // `<-` means "initialise this pinned field in place with this initialiser"
            // (as opposed to `:`, which would move a ready-made value in).
            state <- new_mutex!(State {
                ticks: 0,
                history: KVec::new(),
            }),
            // Code blocks run after the fields above are ready; `?` aborts the load on error.
            // The `_:` means this is not a field, just extra code run during initialisation.
            _: {
                for i in 1..=INIT_TICKS {
                    // Take the lock. The returned *guard* gives access to the `State` and
                    // releases the lock automatically when it goes out of scope (at the end
                    // of each loop iteration). There is no `unlock()` to forget.
                    let mut state = state.lock();
                    state.ticks += 1;
                    let ticks = state.ticks;
                    // Allocation can fail in the kernel: `push` takes GFP flags and returns Result.
                    // `GFP_KERNEL` means "normal allocation; you may sleep while the kernel
                    // frees up memory for me", which is fine here because `init` runs in a
                    // context where sleeping is allowed.
                    // If this returns an error, `?` returns it and `insmod` fails (e.g. with
                    // `ENOMEM`, "out of memory").
                    state.history.push(ticks * i, GFP_KERNEL)?;
                }
            },
        })
    }
}

// The pinned version of `Drop`. It runs at `rmmod`, before the fields are freed. After it
// returns, Rust automatically drops each field too: the mutex is destroyed and the `KVec`'s
// memory is returned to the kernel. We never call a "free" function ourselves.
#[pinned_drop]
impl PinnedDrop for Counter {
    // `self: Pin<&mut Self>` is `&mut self` for a pinned value: we may use the fields, but we
    // cannot move the struct.
    fn drop(self: Pin<&mut Self>) {
        // Even here we must lock to read the data: the same rule applies everywhere.
        let state = self.state.lock();
        // `{:?}` prints a value in "debug" format; for the vector that is `[1, 4, 9]`.
        pr_info!(
            "counter: exit, ticks={} history={:?}\n",
            state.ticks,
            state.history
        );
    }
}
