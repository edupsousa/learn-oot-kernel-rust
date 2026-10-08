// SPDX-License-Identifier: GPL-2.0

//! Lesson 1: our first Rust kernel module.

use kernel::prelude::*;

module! {
    type: Hello,
    name: "hello",
    authors: ["edupsousa"],
    description: "Lesson 1: hello module",
    license: "GPL",
}

struct Hello;

impl kernel::Module for Hello {
    // Runs at `insmod`. Returning `Err` makes the load fail.
    fn init(_module: &'static ThisModule) -> Result<Self> {
        pr_info!("hello: Hello from Rust in the kernel!\n");
        Ok(Hello)
    }
}

impl Drop for Hello {
    // Runs at `rmmod`.
    fn drop(&mut self) {
        pr_info!("hello: Goodbye!\n");
    }
}
