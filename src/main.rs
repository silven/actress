#![feature(async_await, arbitrary_self_types, weak_counts)]

#![allow(unused_imports)]

use std::io;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::rc::{Rc};

use futures::{Poll, Async};

use futures::future::Future;
use futures::future::lazy;
use futures::executor;
use futures::task::{Spawn};
use futures::stream::Stream;

use tokio_threadpool::{ThreadPool, Sender};
use tokio_sync::{oneshot, mpsc};


use std::task::{Context};
use std::pin::Pin;
use std::time::Duration;

use std::marker::PhantomData;
use std::sync::{Arc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};

trait Message: 'static {
    type Result;
}

trait Handle<M: Message> {
    fn accept(&mut self, msg: M, cx: &mut InnerContext) -> M::Result;
}

trait Actor: Send + 'static {
    fn starting(&mut self) {}
    fn started(&mut self) {}

    fn stopping(&mut self) {}
    fn stopped(&mut self) {}
}

trait EnvelopeProxy {
    type Actor: Actor;

    fn accept(&mut self, actor: &mut Self::Actor, cx: &mut InnerContext);
}

struct Envelope<A: Actor>(Box<EnvelopeProxy<Actor = A>>);

unsafe impl<A> Send for Envelope<A> where A: Actor {}

struct EnvelopeInner<A, M> where M: Message, A: Actor + Handle<M> {
    act: PhantomData<*const A>,
    msg: Option<M>,
    reply: Option<oneshot::Sender<M::Result>>,
}

impl<A, M> EnvelopeProxy for EnvelopeInner<A, M> where A: Actor + Handle<M>, M: Message {
    type Actor = A;

    fn accept(&mut self, actor: &mut Self::Actor, cx: &mut InnerContext)  {
        if let Some(msg) = self.msg.take() {
            let result = <Self::Actor as Handle<M>>::accept(actor, msg, cx);

            if let Some(tx) = self.reply.take() {
                tx.send(result); // TODO; What to do if we can't send a reply?
            }
        } else {
            panic!("No message?");
        }
    }
}

impl<A> Envelope<A> where A: Actor {
    fn new<M>(msg: M) -> Self where A: Handle<M>, M: Message {
        Envelope(Box::new(EnvelopeInner {
            act: PhantomData,
            msg: Some(msg),
            reply: None,
        }))
    }

    fn with_reply<M>(msg: M, reply_to: oneshot::Sender<M::Result>) -> Self
        where A: Handle<M>, M: Message
    {
        Envelope(Box::new(EnvelopeInner {
            act: PhantomData,
            msg: Some(msg),
            reply: Some(reply_to),
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
}

#[derive(Debug)]
enum MailboxSendError {
    CouldNotSend,
}

#[derive(Debug)]
enum MailboxAskError {
    CouldNotSend,
    CouldNotRecv,
}

impl<A> Mailbox<A> where A: Actor {
    fn new(inbox: mpsc::UnboundedSender<Envelope<A>>) -> Self {
        Mailbox {
            tx: inbox,
        }
    }

    // Can't use Clone for &Mailbox due to blanket impl
    fn copy(&self) -> Self {
        Mailbox::new(self.tx.clone())
    }

    fn send<M>(&self, msg: M) -> Result<(), MailboxSendError> where A: Actor + Handle<M>, M: Message {
        let env = Envelope::new(msg);
        let mut tx = self.tx.clone();
        match tx.try_send(env) {
            Ok(()) => Ok(()),
            Err(_) => Err(MailboxSendError::CouldNotSend),
        }
    }

    fn ask<M>(&self, msg: M) -> Result<M::Result, MailboxAskError> where A: Actor + Handle<M>, M: Message {
        let (tx, rx) = oneshot::channel();
        let env = Envelope::with_reply(msg, tx);
        let mut tx = self.tx.clone();

        return match tx.try_send(env) {
            Ok(()) => match rx.wait() {  // TODO Can we do without the wait() call?
                Ok(response) => Ok(response),
                Err(_) => Err(MailboxAskError::CouldNotRecv),
            }
            Err(_) => Err(MailboxAskError::CouldNotSend),
        }
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

impl Message for String {
    type Result = String;
}

impl Handle<String> for Dummy {
    fn accept(&mut self, msg: String, cx: &mut InnerContext) -> String {
        self.str_count += 1;
        println!("I got a string message, {}, {}/{}", msg, self.str_count, self.int_count);

        if &msg == "overflow" {
            match cx.spawn_actor(Dummy::new()) {
                Some(mailbox) => {
                    mailbox.send(msg.clone());
                },
                None => {
                    println!("I could not spawn overflower!!");
                },
            }
        }

        if &msg == "mer" {
            match cx.spawn_actor(Dummy::new()) {
                Some(mailbox) => {
                    mailbox.send("hejsan!".to_owned());
                    println!("Spawned something!");
                },
                None => {
                    println!("I could not spawn!");
                },
            }
        }

        return msg;
    }
}

impl Message for usize {
    type Result = usize;
}

impl Handle<usize> for Dummy {
    fn accept(&mut self, msg: usize, cx: &mut InnerContext) -> usize {
        self.int_count += 1;
        println!("I got a numerical message, {} {}/{}", msg, self.str_count, self.int_count);
        if self.int_count >= 100 {
            println!("I am stopping now..");
            cx.state = ActorState::Stopping;
        }
        return msg;
    }
}

struct System {
    started: bool,
    threadpool: ThreadPool,
    context: SystemContext,
    registry: HashMap<String, Box<dyn Any>>,
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


#[derive(Clone)]
struct SystemContext {
    spawner: Sender,
}

impl SystemContext {
    fn new(spawner: Sender) -> Self {
        SystemContext {
            spawner: spawner
        }
    }

    fn spawn_future<F: Future<Item=(), Error=()> + Send + 'static>(&self, fut: F) -> bool {
        self.spawner.spawn(fut).is_ok()
    }

    fn spawn_actor<'s, 'a, A: 'a>(&self, actor: A) -> Result<Mailbox<A>, ()>
        where A: Actor
    {
        let (tx, rx) = mpsc::unbounded_channel();

        let bundle = ActorBundle {
            actor: actor,
            recv: rx,
            inner: InnerContext {
                state: ActorState::Started,
                system: self.clone(),
            },
        };

        match self.spawn_future(bundle) {
            true => Ok(Mailbox::<A>::new(tx)),
            false => Err(()),
        }
    }
}

struct InnerContext {
    state: ActorState,
    system: SystemContext,
}

impl InnerContext {
    fn spawn_actor<A>(&mut self, actor: A) -> Option<Mailbox<A>> where A: Actor {
        match self.system.spawn_actor(actor) {
            Ok(mailbox) => Some(mailbox),
            Err(_) => None,
        }
    }
}

impl<A> Future for ActorBundle<A> where A: Actor {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop { // TODO: this loop shouldnt have to be here.
            match self.recv.poll() {
                Ok(Async::Ready(Some(mut msg))) => {
                    let _ = msg.accept(&mut self.actor, &mut self.inner);
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

impl System {
    fn new() -> Self {
        let pool = ThreadPool::new();
        let spawner = pool.sender().clone();
        System {
            started: false,
            threadpool: pool,
            context: SystemContext::new(spawner),
            registry: HashMap::new(),
        }
    }

    fn start<A>(&mut self, actor: A) -> Mailbox<A> where A: Actor {
        let (tx, rx) = mpsc::unbounded_channel();

        let bundle = ActorBundle {
            actor: actor,
            recv: rx,
            inner: InnerContext {
                state: ActorState::Started,
                system: self.context.clone(),
            },
        };

        self.context.spawn_future(bundle);

        Mailbox::<A>::new(tx)
    }

    // TODO; Move registry to SystemContext to make available to Actors
    fn register<A>(&mut self, name: &str, actor: A) where A: Actor {
        let mailbox = self.start(actor);
        self.registry.insert(name.to_string(), Box::new(mailbox));
    }

    fn find<A>(&self, name: &str) -> Option<Mailbox<A>> where A: Actor {
        if let Some(anybox) = self.registry.get(name) {
            if let Some(mailbox) = anybox.downcast_ref::<Mailbox<A>>() {
                return Some(mailbox.copy());
            }
        }
        None
    }

    fn run_until_completion(mut self) {
        println!("Waiting for system to stop...");
        self.threadpool.shutdown_on_idle().wait().unwrap();
        println!("Done with system?");
    }

    fn spawn_future<F: Future<Item=(), Error=()> + Send + 'static>(&self, fut: F) {
        self.context.spawn_future(fut);
    }
}

fn main() {
    let mut sys = System::new();

    sys.register("dummy", Dummy::new());
    let act = sys.find::<Dummy>("dummy").unwrap();

    //act.send("overflow".to_owned());
    for x in 0..50 {
        let act = sys.start(Dummy::new());
        //let act = sys.find::<Dummy>("dummy").unwrap();
        for _ in 0..1000 {
            act.send("Hej på dig".to_string());
            act.send("Hej på dig igen".to_string());
            act.send(12);
        }
    }

    sys.spawn_future(lazy(move || {
        std::thread::sleep(Duration::from_secs(1));
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
}
