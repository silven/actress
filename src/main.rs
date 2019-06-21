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

struct Envelope<A: Actor + 'static>(Box<EnvelopeProxy<Actor = A>>);

unsafe impl<A> Send for Envelope<A> where A: Actor{}

struct EnvelopeInner<A, M> where M: Message, A: Actor + Handle<M> {
    act: PhantomData<*const A>,
    msg: Option<M>,
    reply: Option<oneshot::Sender<M::Result>>,
}

impl<A, M> EnvelopeProxy for EnvelopeInner<A, M> where A: Actor + Handle<M>, M: Message {
    type Actor = A;

    fn accept(&mut self, actor: &mut Self::Actor, cx: &mut InnerContext)  {
        if let Some(msg) = self.msg.take() {
            let x = <Self::Actor as Handle<M>>::accept(actor, msg, cx);
            // TODO; Reply here
            if let Some(tx) = self.reply.take() {
                tx.send(x);
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
    //tx: cb_channel::Sender<Envelope<A>>,
}

unsafe impl<A> Send for Mailbox<A> where A: Actor {}


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
    //fn new(inbox: cb_channel::Sender<Envelope<A>>) -> Self {
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

        if msg == "mer" {
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
        if self.int_count > 10 {
            println!("I am stopping now..");
            cx.state = ActorState::Stopping;
        }
        return msg;
    }
}

struct System {
    started: bool,
    context: Arc<SystemContext>,
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


struct SystemContext {
    threadpool: ThreadPool,
}

impl SystemContext {
    fn new() -> Self {
        SystemContext {
            threadpool: ThreadPool::new(),
        }
    }

    fn spawn_future<F: Future<Item=(), Error=()> + Send + 'static>(&self, fut: F) {
        self.threadpool.spawn(fut);
    }

    fn spawn_actor<'s, 'a, A: 'a>(self: &Arc<SystemContext>, actor: A) -> Mailbox<A> where A: Actor {
        let (tx, rx) = mpsc::unbounded_channel();

        let bundle: Box<dyn ActorContext> = Box::new(ActorBundle {
            actor: actor,
            recv: rx,
            inner: InnerContext {
                state: ActorState::Started,
                system: Arc::downgrade(self),
            },
        });

        self.spawn_future(bundle);

        Mailbox::<A>::new(tx)
    }
}

struct InnerContext {
    state: ActorState,
    system: Weak<SystemContext>,
}

impl InnerContext {
    fn spawn_actor<A>(&mut self, actor: A) -> Option<Mailbox<A>> where A: Actor {
        println!("Stuff: {:?}/{:?}", self.system.strong_count(), self.system.weak_count());
        if let Some(system) = self.system.upgrade() {
            Some(system.spawn_actor(actor))
        } else {
            None
        }
    }
}

impl<A> ActorContext for ActorBundle<A> where A: Actor {
    fn inner_poll(&mut self) -> Poll<(), ()> {
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
            context: Arc::new(SystemContext::new()),
            actors: Vec::new(),
            registry: HashMap::new(),
        }
    }

    fn start<'s, 'a, A: 'a>(&'s mut self, actor: A) -> Mailbox<A> where A: Actor {
        let (tx, rx) = mpsc::unbounded_channel();
        let bundle: Box<dyn ActorContext> = Box::new(ActorBundle {
            actor: actor,
            recv: rx,
            inner: InnerContext{
                state: ActorState::Started,
                system: Arc::downgrade(&self.context),
            },
        });

        if self.started {
            self.context.spawn_future(bundle);
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
        for actor in self.actors.drain(..) {
            println!("Spawning an actor, lol");
            self.context.spawn_future(actor);
        }

        println!("Waiting for system do stop...");
        //std::thread::sleep(Duration::from_secs(5));
        // TODO: Put this in a loop and spin until unique?
        if let Ok(context) = Arc::try_unwrap(self.context) {
            context.threadpool.shutdown().wait().unwrap();
        } else {
            panic!("What the hell?");
        }
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
        for _x in 0..8 {
            std::thread::sleep(Duration::from_millis(100));
            match act.ask(13000) {
                Ok(r) => println!("Got an {}", r),
                Err(e) => println!("No reply {:?}", e),
            }
        }

        match act.send("mer".to_owned()) {
            Ok(_) => (),
            Err(e) => println!("Could not send final msg"),
        }

        Ok(())
    }));

    sys.run_until_completion();
}
