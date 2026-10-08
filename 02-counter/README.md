<!-- SPDX-License-Identifier: GPL-2.0 -->
# Lesson 2: `02-counter`

Module state: kernel `Mutex<T>`, pin-init (`#[pin_data]`, `try_pin_init!`), `InPlaceModule`, `PinnedDrop`,
fallible allocation (`KVec`), failing `init`. Module parameters are not available in Rust on v6.18.

Back to the [lessons index](../README.md) (setup, VM helpers and dependencies are there).

## Run

```
kmake && vm-run counter.ko
# in the VM:
rmmod counter
```

Expect `counter: exit, ticks=3 history=[1, 4, 9]`.
