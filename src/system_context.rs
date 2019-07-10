use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio_sync::mpsc;
//use tokio_threadpool::Sender;

use crate::internal_handlers::{StoppableActor, Supervises};
use crate::supervisor::SupervisorGuard;
use crate::system::ActorBundle;
use crate::{Actor, ActorContext, Mailbox};

//use tokio_threadpool::Sender;

#[derive(Clone)]
pub(crate) struct SystemContext {
    pub(crate) spawner: tokio::runtime::TaskExecutor,
    //pub(crate) spawner: Sender,
    pub(crate) registry: Arc<Mutex<HashMap<String, Box<dyn StoppableActor>>>>,
    id_counter: usize,
}

impl SystemContext {
    pub(crate) fn new(spawner: tokio::runtime::TaskExecutor) -> Self {
    //pub(crate) fn new(spawner: Sender) -> Self {
        SystemContext {
            spawner: spawner,
            registry: Arc::new(Mutex::new(HashMap::new())),
            id_counter: 0,
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

    pub(crate) fn spawn_future<F: Future<Output = ()> + Send + 'static>(&self, fut: F) {
        self.spawner.spawn(fut) // TODO; Better way of handling spawn errors? (panic?)
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
        self.id_counter += 1;

        #[cfg(feature = "actress_peek")]
        {
            let listeners = Arc::new(Mutex::new(HashMap::new()));
            let mailbox = Mailbox::<A>::new(self.id_counter, tx, Arc::downgrade(&listeners));

            let mut bundle = ActorBundle {
                actor: actor,
                recv: Some(rx),
                supervisor: SupervisorGuard::new(self.id_counter, sup),
                listeners: listeners,
                inner: ActorContext::new(mailbox.copy(), self.clone()),
            };

            // TODO; Figure out a way to move this into the true-branch below
            Actor::started(&mut bundle.actor);
            self.spawn_future(bundle);
            Ok(mailbox)
        }

        #[cfg(not(feature = "actress_peek"))]
        {
            let mailbox = Mailbox::<A>::new(self.id_counter, tx);

            let mut bundle = ActorBundle {
                actor: actor,
                recv: Some(rx),
                supervisor: SupervisorGuard::new(self.id_counter, sup),
                inner: ActorContext::new(mailbox.copy(), self.clone()),
            };

            // TODO; Figure out a way to move this into the true-branch below
            Actor::started(&mut bundle.actor);

            match self.spawn_future(bundle) {
                true => Ok(mailbox),
                false => Err(()),
            }
        }
    }
}
