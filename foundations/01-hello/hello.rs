// SPDX-License-Identifier: GPL-2.0

//! Lesson 1: our first Rust kernel module.
//!
//! # What is a kernel module?
//!
//! The Linux kernel is the core of the operating system: it runs with full access to the
//! hardware and to all memory. A *module* is a piece of kernel code compiled into a separate
//! `.ko` file ("kernel object") that can be **loaded into the running kernel** (`insmod`) and
//! **removed again** (`rmmod`) without rebooting. Drivers are usually modules.
//!
//! Because module code runs *inside* the kernel, a bug here is not "the program crashed", it
//! can be "the whole machine crashed". That is why we only ever load our modules in a
//! throwaway virtual machine, and why Rust's safety guarantees are attractive for this job.
//!
//! # Life of this module
//!
//! ```text
//!  you type                      what the kernel does
//!  --------                      --------------------
//!  insmod hello.ko     ──────▶   loads the file, calls `Hello::init`   (prints "Hello ...")
//!  (module stays loaded, nothing else happens in this lesson)
//!  rmmod hello         ──────▶   drops the `Hello` value → `Hello::drop` (prints "Goodbye!")
//! ```
//!
//! Kernel code cannot use `println!` (there is no terminal, no standard library). Instead it
//! writes to the kernel *log*, a ring buffer in memory that you read with the `dmesg` command.
//!
//! # Where does `main` go?
//!
//! There is none. A module is not a program: it has no `main` and does not run on its own.
//! It provides an "entry" function that the kernel calls when the module is loaded, and the
//! cleanup happens when the module is unloaded.

// The kernel crate is the Rust API over the C kernel (logging, locks, devices, ...).
// `prelude` brings the most commonly needed names into scope: `pr_info!`, `Result`, `Error`,
// `ThisModule`, and so on, so we do not have to import each one by hand.
use kernel::prelude::*;

// `module!` is a macro that fills in everything the kernel expects from a `.ko` file:
//   - the metadata shown by `modinfo` (name, author, description, license);
//   - the C-visible `init_module` / `cleanup_module` functions that the kernel actually calls
//     on load and unload; they forward to the Rust code below.
module! {
    // The Rust type that represents "this module while it is loaded" (defined just below).
    type: Hello,
    // The name used by `lsmod` and `rmmod`. It should match the file name (`hello.ko`).
    name: "hello",
    authors: ["edupsousa"],
    description: "Lesson 1: hello module",
    // Important: the kernel is GPL. A module that declares a non-GPL license "taints" the
    // kernel (it is flagged as possibly unsupported) and may not use GPL-only functions.
    license: "GPL",
}

/// The state of our module while it is loaded.
///
/// It has no fields because this module has nothing to remember. A value of this type is
/// created when the module loads and destroyed when it unloads, so its lifetime *is* the
/// module's lifetime.
struct Hello;

// `kernel::Module` is the trait (Rust's word for "interface") every simple module implements.
// It tells the kernel how to create the module's state at load time.
impl kernel::Module for Hello {
    // Runs at `insmod`.
    //
    // - `_module` is a handle to the module's own metadata. We do not need it, and the leading
    //   underscore tells the compiler "unused on purpose".
    // - The return type `Result<Self>` means "either the new `Hello`, or an error". Returning
    //   `Err(...)` makes `insmod` fail with that error and the module is not loaded. That is
    //   the right way to report problems (out of memory, hardware missing, ...) instead of
    //   crashing.
    fn init(_module: &'static ThisModule) -> Result<Self> {
        // `pr_info!` writes an "info"-level line to the kernel log. It works like `println!`.
        // The `\n` is required: the kernel does not add the newline for you. We start the
        // text with the module name so we can find our lines with `dmesg | grep hello`.
        pr_info!("hello: Hello from Rust in the kernel!\n");
        // Success: hand the kernel our module state. The module is now loaded.
        Ok(Hello)
    }
}

// Rust calls `Drop::drop` automatically when a value goes away. The kernel keeps our `Hello`
// alive until `rmmod`, then drops it, so `drop` doubles as the module's "exit" function.
// There is no separate cleanup function to write, and nothing to forget to call.
impl Drop for Hello {
    // Runs at `rmmod`.
    fn drop(&mut self) {
        pr_info!("hello: Goodbye!\n");
    }
}
