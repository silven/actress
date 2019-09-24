use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering::AcqRel};
use std::sync::{Arc, Mutex};
use tokio_sync::mpsc;

use crate::internal_handlers::{StoppableActor, Supervises};
use crate::supervisor::SupervisorGuard;
use crate::system::ActorBundle;
use crate::{Actor, ActorContext, Mailbox};

//use tokio_threadpool::Sender;

//use tokio_threadpool::Sender;

#[derive(Clone)]
pub(crate) struct SystemContext {
    pub(crate) spawner: tokio::runtime::TaskExecutor,
    //pub(crate) spawner: Sender,
    pub(crate) registry: Arc<Mutex<HashMap<String, Box<dyn StoppableActor>>>>,
    id_counter: Arc<AtomicU64>,
    live_actors: Arc<Mutex<HashMap<u64, String>>>,
}

impl SystemContext {
    pub(crate) fn new(spawner: tokio::runtime::TaskExecutor) -> Self {
        //pub(crate) fn new(spawner: Sender) -> Self {
        SystemContext {
            spawner: spawner,
            registry: Arc::new(Mutex::new(HashMap::new())),
            id_counter: Arc::new(AtomicU64::new(0)),
            live_actors: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn dump(&self) {
        if let Ok(live_data) = self.live_actors.lock() {
            for (k, v) in live_data.iter() {
                println!("Actor {} of type {} is still alive", k, v);
            }
        }
    }

    pub(crate) fn register<A>(&mut self, name: &str, actor: A) -> Mailbox<A>
    where
        A: Actor,
    {
        let mailbox = self.spawn_actor(actor, None).unwrap();

        if let Ok(mut registry) = self.registry.lock() {
            registry.insert(name.to_string(), Box::new(mailbox.copy()));
        }
        mailbox
    }

    pub(crate) fn find<A>(&self, name: &str) -> Option<Mailbox<A>>
    where
        A: Actor,
    {
        if let Ok(registry) = self.registry.lock() {
            if let Some(mailboxbox) = registry.get(name) {
                if let Some(mailbox) = mailboxbox.downcast_ref::<Mailbox<A>>() {
                    return Some(mailbox.copy());
                }
            }
        }
        None
    }

    pub(crate) fn spawn_future<F: Future<Output = ()> + Send + 'static>(&mut self, fut: F) {
        self.spawner.spawn(fut); // TODO; Handle spawn errors
    }

    pub(crate) fn notify_stop(&self, actor_id: u64) {
        if let Ok(mut live_data) = self.live_actors.lock() {
            live_data.remove(&actor_id);
        }
    }

    pub(crate) fn spawn_actor<A>(
        &mut self,
        actor: A,
        sup: Option<Box<dyn Supervises<A>>>,
    ) -> Result<Mailbox<A>, ()>
    where
        A: Actor,
    {
        let (tx, rx) = mpsc::unbounded_channel();
        let actor_id = self.id_counter.fetch_add(1, AcqRel);
        let actor_name = unsafe { std::intrinsics::type_name::<A>() }.to_owned();
        println!(
            "Spawning actor of type '{}' with id '{}'",
            actor_name, actor_id
        );

        if let Ok(mut live_data) = self.live_actors.lock() {
            live_data.insert(actor_id, actor_name);
        }

        #[cfg(feature = "actress_peek")]
        {
            let listeners = Arc::new(Mutex::new(HashMap::new()));
            let mailbox = Mailbox::<A>::new(actor_id, tx, Arc::downgrade(&listeners));

            let mut bundle = ActorBundle {
                actor: actor,
                recv: Some(rx),
                supervisor: SupervisorGuard::new(actor_id, sup),
                listeners: listeners,
                inner: ActorContext::new(mailbox.copy(), self.clone()),
                actor_started: false,
            };

            Actor::starting(&mut bundle.actor);
            self.spawn_future(bundle);
            Ok(mailbox)
        }

        #[cfg(not(feature = "actress_peek"))]
        {
            let mailbox = Mailbox::<A>::new(self.id_counter, tx);

            let mut bundle = ActorBundle {
                actor: actor,
                recv: Some(rx),
                supervisor: SupervisorGuard::new(actor_id, sup),
                inner: ActorContext::new(mailbox.copy(), self.clone()),
                actor_started: false,
            };

            Actor::starting(&mut bundle.actor);
            self.spawn_future(bundle);
            Ok(mailbox)
        }
    }
}
