#![feature(async_await)]

use std::collections::HashSet;
use std::time::Duration;

use actress::{
    Actor, ActorContext, AsyncResponse, Handle, Mailbox, Message, PanicData, Supervisor, System,
};

impl Actor for FibberSup {
    fn stopped(&mut self) {
        println!("Oh noes, supervisor stopped?");
    }
}

struct FibberSup {
    active_workers: HashSet<u64>,
}

impl FibberSup {
    fn new() -> Self {
        FibberSup {
            active_workers: HashSet::new(),
        }
    }
}

#[derive(Debug)]
struct FibRequest(u64);

impl Message for FibRequest {
    type Result = ();
}

#[derive(Debug)]
struct FibReply {
    worker: u64,
    result: u64,
}

impl Message for FibReply {
    type Result = ();
}

impl Handle<FibRequest> for FibberSup {
    type Response = ();

    fn accept(&mut self, msg: FibRequest, cx: &mut ActorContext<Self>) {
        let slave = cx
            .spawn_child(FibberWorker {
                master: cx.mailbox(),
            })
            .unwrap();

        //cx.stop();
        self.active_workers.insert(slave.id());
        slave.send(msg);
    }
}

struct FibberWorker {
    master: Mailbox<FibberSup>,
}

impl Actor for FibberWorker {
    fn stopped(&mut self) {
        println!("Oh noes, worker stopped?");
    }
}

impl Handle<FibRequest> for FibberWorker {
    type Response = ();
    fn accept(&mut self, msg: FibRequest, cx: &mut ActorContext<Self>) {
        // This is hard work!
        std::thread::sleep(Duration::from_secs(2));
        // Send reply
        if msg.0 % 2 == 0 {
            cx.stop();
            //panic!("Oh noes, this is a bad number..");
        }
        self.master.send(FibReply {
            worker: cx.id(),
            result: msg.0 + 1,
        });
        cx.stop();
    }
}

impl Handle<FibReply> for FibberSup {
    type Response = ();
    fn accept(&mut self, msg: FibReply, cx: &mut ActorContext<Self>) {
        println!("Got reply from worker! {} ({})", msg.worker, msg.result);
        self.active_workers.remove(&msg.worker);
        if self.active_workers.is_empty() {
            println!("Done waiting for workers.");
            cx.stop();
        }
    }
}

impl Supervisor<FibberWorker> for FibberSup {
    fn worker_stopped(&mut self, worker_id: u64, info: Option<PanicData>) {
        println!("Oh no! Worker with id {} stopped! {:?}", worker_id, info);

        self.active_workers.remove(&worker_id);
        if self.active_workers.is_empty() {
            println!("Done waiting for workers.");
            //cx.stop();
        }
    }
}

fn main() {
    let mut system = System::new();
    let act = system.start(FibberSup::new());

    // We have to move the act-mailbox somewhere or the program won't terminate.
    system.spawn_future(async move {
        act.send(FibRequest(12));
    });

    system.run_until_completion();
}
