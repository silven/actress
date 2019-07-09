# README

*Disclaimer*: This is just a plaything. Seriously.

Actress is an actor framework for Rust, with small processes communicating by sending messages to each other.

Known issues/limitations:
* Actor ids are usize, meaning your program will crash eventually if you spawn a lot of actors.

Non-features:
* Highest levels of performance.

```rust
#![feature(async_await)]
use actress::{Actor, ActorContext, AsyncResponse, Handle, Message, System};

struct Fibber {}

impl Actor for Fibber {}

#[derive(Debug)]
struct FibRequest(u64);

impl Message for FibRequest {
    type Result = u64;
}

impl Handle<FibRequest> for Fibber {
    type Response = AsyncResponse<FibRequest>;

    fn accept(&mut self, msg: FibRequest, cx: &mut ActorContext<Self>) -> Self::Response {
        let slave = cx.spawn_actor(Fibber::new()).unwrap();

        AsyncResponse::from_future(async move {
            if msg.0 < 2 {
                msg.0
            } else {
                let f1 = slave.ask_async(FibRequest(msg.0 - 1));
                let f2 = slave.ask_async(FibRequest(msg.0 - 2));

                f1.await.unwrap() + f2.await.unwrap()
            }
        })
    }
}

fn main() {
    let mut system = System::new();
    let act = system.start(Fibber{});

    // We have to move the act-mailbox somewhere or the program won't terminate.
    system.spawn_future(async move {
        for x in 1..=30 {
            match act.ask(FibRequest(x)) {
                Ok(fib) => println!("The fib({}) = {}", x, fib),
                Err(e) => println!("Could not ask: {:?}", e),
            };
        }
    });

    system.run_until_completion();
}

```
