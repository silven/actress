use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::future::lazy;

use std::time::Duration;

use actress::{
    actor::{Actor, ActorContext, Handle, Message},
    system::System,
};

struct Dummy {
    str_count: usize,
    int_count: usize,
}

impl Dummy {
    fn new() -> Self {
        Dummy {
            str_count: 0,
            int_count: 0,
        }
    }
}

impl Actor for Dummy {}

struct Msg<T>(T);

impl Msg<String> {
    fn new(msg: &str) -> Self {
        Msg(msg.to_owned())
    }
}

impl Msg<usize> {
    fn new(msg: usize) -> Self {
        Msg(msg)
    }
}

impl Message for Msg<String> {
    type Result = String;
}

impl Handle<Msg<String>> for Dummy {
    fn accept(&mut self, msg: Msg<String>, cx: &mut ActorContext) -> String {
        self.str_count += 1;
        println!(
            "I got a string message, {}, {}/{}",
            msg.0, self.str_count, self.int_count
        );

        if &msg.0 == "overflow" {
            match cx.spawn_actor(Dummy::new()) {
                Some(mailbox) => {
                    mailbox.send(Msg::<String>::new(&msg.0));
                }
                None => {
                    println!("I could not spawn overflower!!");
                }
            }
        }

        if &msg.0 == "mer" {
            match cx.spawn_actor(Dummy::new()) {
                Some(mailbox) => {
                    mailbox.send(Msg::<String>::new("hejsan!"));
                    println!("Spawned something!");
                }
                None => {
                    println!("I could not spawn!");
                }
            }
        }

        return msg.0;
    }
}

impl Message for Msg<usize> {
    type Result = usize;
}

impl Handle<Msg<usize>> for Dummy {
    fn accept(&mut self, msg: Msg<usize>, cx: &mut ActorContext) -> usize {
        self.int_count += 1;
        println!(
            "I got a numerical message, {} {}/{}",
            msg.0, self.str_count, self.int_count
        );
        if self.int_count >= 100 {
            println!("I am stopping now..");
            cx.stop();
        }
        return msg.0;
    }
}

fn main() {
    let mut sys = System::new();

    sys.register("dummy", Dummy::new());
    let act = sys.find::<Dummy>("dummy").unwrap();

    let counter = Arc::new(AtomicUsize::new(0));
    let cnt = Arc::clone(&counter);

    act.peek::<Msg<usize>, _>(move |_x| {
        cnt.fetch_add(1, Ordering::SeqCst);
    });

    //act.send("overflow".to_owned());
    for x in 0..50 {
        //let act = sys.start(Dummy::new());
        //act.alter::<Msg<usize>, _>(|x| x + 1);

        //let act = sys.find::<Dummy>("dummy").unwrap();
        for _ in 0..1000 {
            act.send(Msg::<String>::new("Hej på dig"));
            act.send(Msg::<String>::new("Hej på dig igen"));
            act.send(Msg::<usize>::new(12));
        }
    }

    sys.spawn_future(lazy(move || {
        std::thread::sleep(Duration::from_secs(1));
        act.grab::<Msg<usize>, _>(|x| x.0 + 1);

        for _x in 0..100 {
            std::thread::sleep(Duration::from_millis(10));
            match act.ask(Msg::<usize>::new(13000)) {
                Ok(r) => println!("Got an {}", r),
                Err(e) => println!("No reply {:?}", e),
            }
        }

        act.clear_listener::<Msg<usize>>();
        for _x in 0..100 {
            std::thread::sleep(Duration::from_millis(10));
            match act.ask(Msg::<usize>::new(13000)) {
                Ok(r) => println!("Got an {}", r),
                Err(e) => println!("No reply {:?}", e),
            }
        }

        Ok(())
    }));

    sys.run_until_completion();
    println!("The counter is: {}", counter.load(Ordering::SeqCst));
}
