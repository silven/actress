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
    fn accept(&mut self, msg: Msg, cx: &mut ActorContext) -> <Msg as Message>::Result {
        println!("Outside async block, computing {}", msg.0);

        block_on(async move {
            let slave = cx.spawn_actor(Dummy::new()).unwrap();
            println!("Inside async block");
            if msg.0 > 0 {
                let reply = slave.ask_async(Msg(msg.0 - 1)).await;
                //std::thread::sleep(Duration::from_secs(5));
                println!("Done computing, got {:?}", reply);
            }
            println!("No slave computation needed.");
            slave.send(SystemMessage::Stop);
            msg.0
        })
    }
}

use futures::executor::block_on;

fn main() {
    let mut system = System::new();
    let act = system.start(Dummy::new());

    system.spawn_future(async move {
        match act.ask(Msg(5)) {
            Ok(fut) => println!("The result was: {}", fut),
            Err(e) => println!("Could not ask: {:?}", e),
        };

        act.send(SystemMessage::Stop);
    });

    std::thread::sleep(Duration::from_secs(1));
    system.run_until_completion();
}
