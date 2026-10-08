// SPDX-License-Identifier: GPL-2.0

//! Lesson 2: module state, pinned `Mutex` and fallible allocation.

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
struct State {
    ticks: u32,
    history: KVec<u32>,
}

// `#[pin_data]`: a kernel `Mutex` contains a linked list of waiters, so it must never move in
// memory once initialised. Fields marked `#[pin]` are initialised in place.
#[pin_data(PinnedDrop)]
struct Counter {
    #[pin]
    state: Mutex<State>,
}

// `InPlaceModule` (instead of `Module`): `init` returns an *initialiser* rather than a value,
// so the module struct is built directly at its final address.
impl kernel::InPlaceModule for Counter {
    fn init(_module: &'static ThisModule) -> impl PinInit<Self, Error> {
        pr_info!("counter: init\n");

        try_pin_init!(Self {
            state <- new_mutex!(State {
                ticks: 0,
                history: KVec::new(),
            }),
            // Code blocks run after the fields above are ready; `?` aborts the load on error.
            _: {
                for i in 1..=INIT_TICKS {
                    let mut state = state.lock();
                    state.ticks += 1;
                    let ticks = state.ticks;
                    // Allocation can fail in the kernel: `push` takes GFP flags and returns Result.
                    state.history.push(ticks * i, GFP_KERNEL)?;
                }
            },
        })
    }
}

#[pinned_drop]
impl PinnedDrop for Counter {
    fn drop(self: Pin<&mut Self>) {
        let state = self.state.lock();
        pr_info!(
            "counter: exit, ticks={} history={:?}\n",
            state.ticks,
            state.history
        );
    }
}
