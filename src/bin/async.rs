#![feature(async_await)]
use std::future::Future;
use std::time::Duration;

use actress::system::SystemMessage;
use actress::{
    actor::{Actor, ActorContext, Handle, Message},
    system::System,
};

struct Dummy {
    count: u64,
}

impl Dummy {
    fn new() -> Self {
        Dummy { count: 0 }
    }
}

impl Actor for Dummy {}

#[derive(Debug)]
struct Msg(u64);

impl Message for Msg {
    type Result = u64;
}

impl Handle<Msg> for Dummy {
    type Response = std::pin::Pin<Box<dyn std::future::Future<Output=u64> + Send>>;

    fn accept(&mut self, msg: Msg, cx: &mut ActorContext) -> Self::Response {
        println!("Outside async block, computing {}", msg.0);

        let slave = cx.spawn_actor(Dummy::new()).unwrap();
        Box::pin(async move {
            if msg.0 > 0 {
                let r = slave.ask_async(Msg(msg.0 - 1)).await;
                println!("Got a result {:?}", r);
            }
            msg.0
        })
    }
}

use futures::executor::block_on;
use futures::future::FutureObj;

fn main() {
    let mut system = System::new();
    let act = system.start(Dummy::new());

    {
        let worker = system.start(Dummy::new());
        for x in 1..50 {
            match act.ask(Msg(x)) {
                Ok(fut) => println!("The result was: {}", fut),
                Err(e) => println!("Could not ask: {:?}", e),
            };
        }
    }

    system.spawn_future(async move {
        match act.ask(Msg(50)) {
            Ok(fut) => println!("The result was: {}", fut),
            Err(e) => println!("Could not ask: {:?}", e),
        };

        act.send(SystemMessage::Stop);
    });

    std::thread::sleep(Duration::from_secs(1));
    system.run_until_completion();
}