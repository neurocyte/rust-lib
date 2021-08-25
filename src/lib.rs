// TODO: All of the examples here are also inside of the ./examples folder because rustdoc doesn't
//       work with the target wasm32-wasi (https://github.com/rust-lang/cargo/issues/7040) at the
//       moment, but we still want to make sure this code compiles. This duplication is hard to
//       keep in sync and as soon as this issue is closed we should remove the duplicates.

/*!
Helper library for building Rust applications that run on [lunatic][1].

# Main concepts

The main abstraction in [lunatic][1] is a [`Process`](crate::process::Process). Contrary to
operating system processes, lunatic processes are lightweight and fast to spawn. They are
designed for **massive** concurrency.

Processes can be spawned from just a function:
```
use lunatic::{process, Mailbox};

#[lunatic::main]
fn main(m: Mailbox<()>) {
    let (this, m) = process::this(m);
    process::spawn_with(this, |parent, _: Mailbox<()>| {
        println!("Hi! I'm a process.");
        // Notify parent that we are done.
        parent.send(());
    })
    .unwrap();
    // Wait for child to finish. If this line was missing the main process could shut down before
    // the child prints anything. If the main process finishes, all others are killed.
    m.receive();
}
```

One important characteristic of processes is that they are sandboxed. Each of them gets a separate
memory and they can't access any memory from the parent, not even through raw pointer access. If we
need to pass any information to the newly spawned process we can do it through a `context`:
```
use lunatic::{process, Mailbox};

let proc = "Process";
process::spawn_with(proc.to_string(), |proc, _: Mailbox<()>| {
    // This closure gets a new heap and stack to execute on,
    // and can't access the memory of the parent process.
    println!("Hello {}!", proc);
})
.unwrap();
```

## Messaging

Processes can exchange information with each other through [messages](crate::Message):

```
use lunatic::process;

let proc = process::spawn(|mailbox| {
    let message = mailbox.receive();
    println!("Hello {}", message);
})
.unwrap();

proc.send("World!".to_string());
```
Most types should automatically implement the [`Message`] trait, but for custom types a
`#[derive(lunatic::derive::Message)]` helper can be used.

Each process gets a [`Mailbox`] as an argument to the entry function. Mailboxes can be used to
[`receive`](Mailbox::receive()) messages. If there are no messages in the mailbox the process
will block on [`receive`](Mailbox::receive()) until a message arrives.

## Linking

Processes can be linked together. This means that if one of them fails, all the ones linked to
it will get notified. A linked process can be spawned with the [`process::spawn_link`] function.
The function will take the current [`Mailbox`] and return a [`LinkMailbox`], that will also
receive notifications about linked processes. If we would like to automatically fail as soon as
one of the linked processes fails, we can turn the [`LinkMailbox`] back to a regular one with
the `panic_if_link_panics()` function.

```
use lunatic::{process, Mailbox};

#[lunatic::main]
fn main(mailbox: Mailbox<()>) {
    let (_child, mailbox) = process::spawn_link(mailbox, child).unwrap();
    // Wait on message
    assert!(mailbox.receive().is_err());
}

fn child(_: Mailbox<()>) {
    panic!("Error");
}

```

## Sandboxing

A [`Environment`] can define characteristics that processes spawned into it have. The environment
can limit:

* Memory usage
* Compute usage
* WebAssembly host function (syscalls) access

An `Environment` is configured through a [`Config`].

```
use lunatic::{Config, Environment, Mailbox};

#[lunatic::main]
fn main(m: Mailbox<()>) {
    // Create a new environment where processes can use maximum 17 Wasm pages of
    // memory (17 * 64KB) & 1 gallon of instructions (~=100k CPU cycles).
    let mut config = Config::new(1_200_000, Some(1));
    // Allow only syscalls under the "wasi_snapshot_preview1::environ*" namespace
    config.allow_namespace("wasi_snapshot_preview1::environ");
    let mut env = Environment::new(config).unwrap();
    let module = env.add_this_module().unwrap();

    // This process will fail because it can't uses syscalls for std i/o
    let (_, m) = module
        .spawn_link(m, |_: Mailbox<()>| println!("Hi from different env"))
        .unwrap();
    assert!(m.receive().is_err());

    // This process will fail because it uses too much memory
    let (_, m) = module
        .spawn_link(m, |_: Mailbox<()>| {
            vec![0; 150_000];
        })
        .unwrap();
    assert!(m.receive().is_err());

    // This process will fail because it uses too much compute
    let (_, m) = module.spawn_link(m, |_: Mailbox<()>| loop {}).unwrap();
    assert!(m.receive().is_err());
}
```

## Loading other WebAssembly modules

Lunatic allows for dynamic loading of other WebAssembly modules during runtime.
[`Environment::add_module`] can be used to add WebAssembly modules to an environment.

# Setting up Cargo for lunatic

To simplify developing, testing and running lunatic applications with cargo, you can add a
`.cargo/config.toml` file to your project with the following content:

```toml
[build]
target = "wasm32-wasi"

[target.wasm32-wasi]
runner = "lunatic"
```

Now you can just use the commands you were already familiar with, such as `cargo run`, `cargo test`
and cargo is going to automatically build your project as a WebAssembly module and run it inside of
lunatic.

# Debugging

If a process dies, either because an unsupported syscall was called or a `Kill` signal was received
there is not going to be any output in the terminal. To get more insight set the `RUST_LOG`
environment variable to `lunatic=debug`. E.g. `RUST_LOG=lunatic=debug cargo run`.

[1]: https://github.com/lunatic-solutions/lunatic
*/

mod environment;
mod error;
mod host_api;
mod mailbox;
pub mod net;
pub mod process;

pub use environment::{Config, Environment, Module, ThisModule};
pub use error::LunaticError;
pub use mailbox::{LinkMailbox, Mailbox, Signal};

pub use lunatic_macros::main;
pub use lunatic_macros::test;