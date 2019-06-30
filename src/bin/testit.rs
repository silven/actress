#![feature(async_await)]

use std::time::Duration;

use actress::{
    Actor, ActorContext, Handle, Message,
    System, SyncResponse,
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
enum Msg {
    Compute(u64),
    Respond,
}

#[derive(Debug)]
enum Response {
    ComputeResponse,
    Response(u64),
}

impl Message for Msg {
    type Result = Response;
}

impl Handle<Msg> for Dummy {
    type Response = SyncResponse<Msg>;

    fn accept(&mut self, msg: Msg, cx: &mut ActorContext) -> Self::Response {
        let r = match msg {
            Msg::Compute(value) => {
                std::thread::sleep(Duration::from_millis(value));
                self.count += value;
                Response::ComputeResponse
            }
            Msg::Respond => Response::Response(self.count),
        };
        SyncResponse(r)
    }
}

fn main() {
    let mut system = System::new();

    // Services are killed when the system is stopped.
    let act = system.register("dummy", Dummy::new());

    let a = act.copy();

    system.spawn_future(async move {
        for x in 1..100 {
            a.send(Msg::Compute(x));
            std::thread::sleep(Duration::from_micros(x));
        }
    });

    system.spawn_future(async move {
        let x = act.ask(Msg::Respond);
        println!("The result was: {:?}", x);
    });

    std::thread::sleep(Duration::from_secs(1));
    system.run_until_completion();
}
