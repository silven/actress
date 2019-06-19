#![feature(async_await)]

#![allow(unused_imports)]

use std::io;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt::Display;
use std::rc::{Rc, Weak};

use std::marker::Unpin;

use futures::future::{Future, FutureObj};
use futures::future::lazy;
use futures::StreamExt;
use futures::channel::mpsc;
use futures::executor::{self, ThreadPool};
use futures::io::AsyncWriteExt;
use futures::task::{Spawn, SpawnExt};
use futures::stream::Stream;


use std::task::{Context, Poll};
use std::pin::Pin;
use std::time::Duration;


use rand::seq::SliceRandom;

use crossbeam_channel as cb_channel;

use romio::{TcpListener, TcpStream};

use std::marker::PhantomData;
use core::borrow::BorrowMut;
use futures::sink::SinkExt;
use std::thread::spawn;


trait Handle<M> {
    fn accept(&mut self, msg: M);
}

trait Actor: Send + 'static {
    fn id(&self) -> String;
}

trait EnvelopeProxy {
    type Actor: Actor;

    fn accept(&mut self, actor: &mut Self::Actor);
}

struct Envelope<A: Actor + 'static>(Box<EnvelopeProxy<Actor = A>>);

unsafe impl<A> Send for Envelope<A> where A: Actor{}

struct EnvelopeInner<A, M> {
    act: PhantomData<*const A>,
    msg: Option<M>,
}

impl<A, M> EnvelopeProxy for EnvelopeInner<A, M> where A: Actor + Handle<M>, M: 'static  {
    type Actor = A;

    fn accept(&mut self, actor: &mut Self::Actor) {
        if let Some(msg) = self.msg.take() {
            <Self::Actor as Handle<M>>::accept(actor, msg);
        }
    }
}

impl<A> Envelope<A> where A: Actor {
    fn new<M>(msg: M) -> Self where A: Handle<M>, M: 'static {
        Envelope(Box::new(EnvelopeInner {
            act: PhantomData,
            msg: Some(msg),
        }))
    }
}

impl<A> EnvelopeProxy for Envelope<A> where A: Actor {
    type Actor = A;

    fn accept(&mut self, actor: &mut Self::Actor) {
        self.0.accept(actor);
    }
}

#[derive(Clone)]
struct Mailbox<A> where A: Actor {
    tx: mpsc::Sender<Envelope<A>>,
    //tx: cb_channel::Sender<Envelope<A>>,
}

unsafe impl<A> Send for Mailbox<A> where A: Actor {}

impl<A> Mailbox<A> where A: Actor {
    fn new(inbox: mpsc::Sender<Envelope<A>>) -> Self {
    //fn new(inbox: cb_channel::Sender<Envelope<A>>) -> Self {
        Mailbox {
            tx: inbox,
        }
    }

    fn copy(&self) -> Self {
        Mailbox::new(self.tx.clone())
    }

    fn send<M>(&self, msg: M) where A: Actor + Handle<M>, M: 'static  {
        let mut tx = self.tx.clone();
        let env = Envelope::new(msg);

        executor::block_on(tx.send(env));
        //tx.start_send(env).expect("Could not send, ju!");
    }
}

struct Dummy {
    message: String,
}

impl Dummy {
    fn new(inner: &str) -> Self {
        Dummy { message: inner.to_string() }
    }
}

impl Actor for Dummy {
    fn id(&self) -> String {
        format!("Dummy {{ {} }}", self.message)
    }
}

impl Handle<String> for Dummy {
    fn accept(&mut self, msg: String) {
        println!("I got a string message, {}", msg);
    }
}

impl Handle<usize> for Dummy {
    fn accept(&mut self, msg: usize) {
        println!("I got a numerical message, {}", msg);
    }
}

struct System {
    started: bool,
    threadpool: ThreadPool,
    actors: Vec<Box<dyn ActorContext>>,
    registry: HashMap<String, Box<dyn Any>>,
}

trait ActorContext: Send {
    fn inner_poll(&mut self, ctx: &mut Context) -> Poll<()>;
}

impl core::future::Future for System {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        println!("I am a system?");

        let w = cx.waker().clone();

        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            w.wake();
        });


        Poll::Pending
    }
}

struct ActorBundle<A: Actor> {
    actor: A,
    recv: mpsc::Receiver<Envelope<A>>,
    //recv: cb_channel::Receiver<Envelope<A>>,
}

//impl<A> core::marker::Unpin for ActorBundle<A> where A: Actor {}
//unsafe impl<A> Send for ActorBundle<A> where A: Actor {}

impl<A> ActorContext for ActorBundle<A> where A: Actor {

    fn inner_poll(&mut self, cx: &mut Context) -> Poll<()> {

        loop { // TODO: this shouldnt be here.
            match self.recv.poll_next_unpin(cx) {
                Poll::Ready(Some(mut msg)) => {
                    //println!("Working with msg: {}/{}", self.actor.id(), msg);
                    //println!("Got a message!");
                    msg.accept(&mut self.actor);
                },

                Poll::Pending => { return Poll::Pending; },
                Poll::Ready(None) => { return Poll::Ready(()) },
            }
        }

    }
}

impl Future for Box<dyn ActorContext> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        self.inner_poll(cx)
    }
}

impl System {
    fn new() -> Self {
        System {
            started: false,
            threadpool: ThreadPool::new().unwrap(),
            actors: Vec::new(),
            registry: HashMap::new(),
        }
    }

    fn start<'s, 'a, A: 'a>(&'s mut self, actor: A) -> Mailbox<A> where A: Actor {
        //let (tx, rx) = cb_channel::unbounded();
        let (tx, rx) = mpsc::channel(100);
        let bundle: Box<dyn ActorContext> = Box::new(ActorBundle {
            actor: actor,
            recv: rx,
        });

        if self.started {
            self.threadpool.spawn_obj(FutureObj::new(Box::pin(bundle)));
        } else {
            self.actors.push(bundle);
        }
        Mailbox::<A>::new(tx)
    }

    fn register<'s, 'n, A: 'static>(&'s mut self, name: &'n str, actor: A) where A: Actor {
        let mailbox = self.start(actor);
        self.registry.insert(name.to_string(), Box::new(mailbox));
    }

    fn find<'s, 'n, A>(&'s self, name: &'n str) -> Option<Mailbox<A>> where A: Actor {
        if let Some(anybox) = self.registry.get(name) {
            if let Some(mailbox) = anybox.downcast_ref::<Mailbox<A>>() {
                return Some(mailbox.copy());
            }
        }
        None
    }

    fn run(&mut self) {
        let mut pool = self.threadpool.clone();
        for actor in self.actors.drain(..) {
            pool.spawn_obj(FutureObj::new(Box::pin(actor)));
        }

        let pinned = Pin::new(self);
        pool.run(pinned);
/*
        for mut actor in self.actors.drain(..) {
            println!("Starting an actor!");
            threadpool
                .spawn(run_actor(actor))
                .expect("Could not spawn actor.");
        }
*/
        println!("Done with system?");
    }

    fn spawn<F: Future<Output=()> + Send + 'static>(&mut self, fut: F) {
        self.threadpool.spawn(fut);
    }
}

fn main() {
    let mut sys = System::new();

    sys.register("dummy", Dummy::new("Hej"));
    let act = sys.find::<Dummy>("dummy").unwrap();

    for x in 0..50 {
        //let act = sys.start(Dummy::new(&format!("Actor {}", x)));
        //let act = sys.find::<Dummy>("dummy").unwrap();
        for _ in 0..1000 {
            act.send("Hej på dig".to_string());
            act.send("Hej på dig igen".to_string());
            act.send(12);
        }
    }
    /*
    sys.spawn(lazy(move |_| {
        std::thread::sleep(Duration::from_secs(2));
        //act.send(134);
    }));
    */
    sys.run();

    // Not good...
    std::thread::sleep(std::time::Duration::from_secs(1));

}
