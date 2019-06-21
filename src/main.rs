#![feature(async_await)]

#![allow(unused_imports)]

use std::io;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use futures::{Poll, Async};

use futures::future::Future;
use futures::future::lazy;
use futures::executor;
use futures::task::{Spawn};
use futures::stream::Stream;

use tokio_threadpool::{ThreadPool, Sender};
use tokio_sync::mpsc;

use std::task::{Context};
use std::pin::Pin;
use std::time::Duration;

use std::marker::PhantomData;


trait Handle<M> {
    fn accept(&mut self, msg: M, cx: &mut InnerContext);
}

trait Actor: Send + 'static { }

trait EnvelopeProxy {
    type Actor: Actor;

    fn accept(&mut self, actor: &mut Self::Actor, cx: &mut InnerContext);
}

struct Envelope<A: Actor + 'static>(Box<EnvelopeProxy<Actor = A>>);

unsafe impl<A> Send for Envelope<A> where A: Actor{}

struct EnvelopeInner<A, M> {
    act: PhantomData<*const A>,
    msg: Option<M>,
}

impl<A, M> EnvelopeProxy for EnvelopeInner<A, M> where A: Actor + Handle<M>, M: 'static  {
    type Actor = A;

    fn accept(&mut self, actor: &mut Self::Actor, cx: &mut InnerContext) {
        if let Some(msg) = self.msg.take() {
            <Self::Actor as Handle<M>>::accept(actor, msg, cx);
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

    fn accept(&mut self, actor: &mut Self::Actor, cx: &mut InnerContext) {
        self.0.accept(actor, cx);
    }
}

#[derive(Clone)]
struct Mailbox<A> where A: Actor {
    tx: mpsc::UnboundedSender<Envelope<A>>,
    //tx: cb_channel::Sender<Envelope<A>>,
}

unsafe impl<A> Send for Mailbox<A> where A: Actor {}

impl<A> Mailbox<A> where A: Actor {
    fn new(inbox: mpsc::UnboundedSender<Envelope<A>>) -> Self {
    //fn new(inbox: cb_channel::Sender<Envelope<A>>) -> Self {
        Mailbox {
            tx: inbox,
        }
    }

    // Can't use Clone for &Mailbox due to blanket impl
    fn copy(&self) -> Self {
        Mailbox::new(self.tx.clone())
    }

    fn send<M>(&self, msg: M) where A: Actor + Handle<M>, M: 'static {
        let env = Envelope::new(msg);
        let mut tx = self.tx.clone();
        tx.try_send(env);
    }
}

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

impl Actor for Dummy { }

impl Handle<String> for Dummy {
    fn accept(&mut self, msg: String, cx: &mut InnerContext) {
        self.str_count += 1;
        println!("I got a string message, {}, {}/{}", msg, self.str_count, self.int_count);
    }
}

impl Handle<usize> for Dummy {
    fn accept(&mut self, msg: usize, cx: &mut InnerContext) {
        self.int_count += 1;
        println!("I got a numerical message, {} {}/{}", msg, self.str_count, self.int_count);
        if self.int_count > 10 {
            println!("I am stopping now..");
            cx.state = ActorState::Stopping;
        }
    }
}

struct System {
    started: bool,
    threadpool: ThreadPool,
    actors: Vec<Box<dyn ActorContext>>,
    registry: HashMap<String, Box<dyn Any>>,
}

trait ActorContext: Send {
    fn inner_poll(&mut self) -> Poll<(), ()>;
}

#[derive(PartialEq)]
enum ActorState {
    Started,
    Stopping,
    Stopped,
}

struct ActorBundle<A: Actor> {
    actor: A,
    recv: mpsc::UnboundedReceiver<Envelope<A>>,
    inner: InnerContext,
}

struct InnerContext {
    state: ActorState,
}

impl<A> ActorContext for ActorBundle<A> where A: Actor {
    fn inner_poll(&mut self) -> Poll<(), ()> {
        loop { // TODO: this loop shouldnt have to be here.
            match self.recv.poll() {
                Ok(Async::Ready(Some(mut msg))) => {
                    msg.accept(&mut self.actor, &mut self.inner);
                    if self.inner.state == ActorState::Stopping {
                        // Is this how we stop?
                        self.recv.close();
                        break Ok(Async::Ready(()));
                    }
                },

                Ok(Async::Ready(None)) => { return Ok(Async::Ready(())) },
                _ => { return Ok(Async::NotReady) },
            }
        }
    }
}

impl Future for Box<dyn ActorContext> {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.inner_poll()
    }
}

impl System {
    fn new() -> Self {
        System {
            started: false,
            threadpool: ThreadPool::new(),
            actors: Vec::new(),
            registry: HashMap::new(),
        }
    }

    fn start<'s, 'a, A: 'a>(&'s mut self, actor: A) -> Mailbox<A> where A: Actor {
        //let (tx, rx) = cb_channel::unbounded();
        let (tx, rx) = mpsc::unbounded_channel();
        let bundle: Box<dyn ActorContext> = Box::new(ActorBundle {
            actor: actor,
            recv: rx,
            inner: InnerContext{ state: ActorState::Started },
        });

        if self.started {
            self.threadpool.spawn(bundle);
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

    fn run_until_completion(mut self) {
        let c = self.threadpool.sender().clone();

        for actor in self.actors.drain(..) {
            println!("Spawning an actor, lol");
            self.threadpool.spawn(actor);
        }

        println!("Waiting for system do stop...");
        self.threadpool.shutdown().wait().unwrap();
        println!("Done with system?");
    }

    fn spawn<F: Future<Item=(), Error=()> + Send + 'static>(&self, fut: F) {
        self.threadpool.spawn(fut);
    }
}

fn main() {
    let mut sys = System::new();

    sys.register("dummy", Dummy::new());
    let act = sys.find::<Dummy>("dummy").unwrap();

    for x in 0..50 {
        let act = sys.start(Dummy::new());
        //let act = sys.find::<Dummy>("dummy").unwrap();
        for _ in 0..1000 {
            act.send("Hej på dig".to_string());
            act.send("Hej på dig igen".to_string());
            act.send(12);
        }
    }

    sys.spawn(lazy(move || {
        std::thread::sleep(Duration::from_secs(1));
        for _x in 0..100 {
            std::thread::sleep(Duration::from_millis(100));
            act.send(13000);
        }
        Ok(())
    }));

    sys.run_until_completion();
}
