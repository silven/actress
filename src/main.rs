#![feature(async_await, arbitrary_self_types, weak_counts)]
#![allow(unused_imports)]

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::io;
use std::rc::Rc;

use futures::{Async, Poll};

use futures::executor;
use futures::future::lazy;
use futures::future::Future;
use futures::stream::Stream;
use futures::task::Spawn;

use tokio_sync::{mpsc, oneshot};
use tokio_threadpool::{Sender, ThreadPool};

use std::pin::Pin;
use std::task::Context;
use std::time::Duration;

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};


mod actor;
use actor::{Actor, Handle, Message};

mod mailbox;

mod system;
use system::{System};
use crate::actor::ActorContext;


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

impl Message for String {
    type Result = String;
}

impl Handle<String> for Dummy {
    fn accept(&mut self, msg: String, cx: &mut ActorContext) -> String {
        self.str_count += 1;
        println!(
            "I got a string message, {}, {}/{}",
            msg, self.str_count, self.int_count
        );

        if &msg == "overflow" {
            match cx.spawn_actor(Dummy::new()) {
                Some(mailbox) => {
                    mailbox.send(msg.clone());
                }
                None => {
                    println!("I could not spawn overflower!!");
                }
            }
        }

        if &msg == "mer" {
            match cx.spawn_actor(Dummy::new()) {
                Some(mailbox) => {
                    mailbox.send("hejsan!".to_owned());
                    println!("Spawned something!");
                }
                None => {
                    println!("I could not spawn!");
                }
            }
        }

        return msg;
    }
}

impl Message for usize {
    type Result = usize;
}

impl Handle<usize> for Dummy {
    fn accept(&mut self, msg: usize, cx: &mut ActorContext) -> usize {
        self.int_count += 1;
        println!(
            "I got a numerical message, {} {}/{}",
            msg, self.str_count, self.int_count
        );
        if self.int_count >= 100 {
            println!("I am stopping now..");
            cx.stop();
        }
        return msg;
    }
}

fn main() {
    let mut sys = System::new();

    sys.register("dummy", Dummy::new());
    let act = sys.find::<Dummy>("dummy").unwrap();

    let counter = Arc::new(AtomicUsize::new(0));
    let cnt = Arc::clone(&counter);

    act.peek::<usize, _>(move |_x| { cnt.fetch_add(1, Ordering::SeqCst); } );

    //act.send("overflow".to_owned());
    for x in 0..50 {
        //let act = sys.start(Dummy::new());
        //act.alter::<usize, _>(|x| x + 1);

        //let act = sys.find::<Dummy>("dummy").unwrap();
        for _ in 0..1000 {
            act.send("Hej på dig".to_string());
            act.send("Hej på dig igen".to_string());
            act.send(12);
        }
    }

    sys.spawn_future(lazy(move || {
        std::thread::sleep(Duration::from_secs(1));
        act.grab::<usize, _>(|x| x + 1);

        for _x in 0..100 {
            std::thread::sleep(Duration::from_millis(10));
            match act.ask(13000) {
                Ok(r) => println!("Got an {}", r),
                Err(e) => println!("No reply {:?}", e),
            }
        }

        act.clear_listener::<usize>();
        for _x in 0..100 {
            std::thread::sleep(Duration::from_millis(10));
            match act.ask(13000) {
                   Ok(r) => println!("Got an {}", r),
                Err(e) => println!("No reply {:?}", e),
            }
        }

        Ok(())
    }));

    sys.run_until_completion();
    println!("The counter is: {}", counter.load(Ordering::SeqCst));
}
