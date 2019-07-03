use std::panic::PanicInfo;
use std::sync::Arc;

use crossbeam::atomic::AtomicCell;

use crate::{Actor, Mailbox};
use crate::internal_handlers::{SupervisedBy, Supervises};

pub trait Supervisor<W>: Actor where W: Actor {
    fn worker_stopped(&mut self, worker_id: usize, info: Option<PanicData>);
}

#[derive(Debug)]
pub struct PanicData {
    message: Option<String>,
    file: String,
    line: u32,
    column: u32,
}

impl From<&std::panic::PanicInfo<'_>> for PanicData {
    fn from(value: &std::panic::PanicInfo) -> Self {
        // Current impl always returns Some
        let loc = value.location().unwrap();
        PanicData {
            message: value.payload().downcast_ref::<&str>().map(ToString::to_string),
            file: loc.file().to_owned(),
            line: loc.line(),
            column: loc.column(),
        }

    }
}

pub(crate) struct PanicHookGuard(Option<Box<dyn Fn(&PanicInfo) + Sync + Send + 'static>>);

impl PanicHookGuard {
    pub(crate) fn new(hook: Box<dyn Fn(&PanicInfo) + Sync + Send + 'static>) -> Self {
        PanicHookGuard(Some(hook))
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        // Never none
        std::panic::set_hook(self.0.take().unwrap());
    }
}


pub(crate) struct ChildGuard<A>(Vec<Box<dyn SupervisedBy<A>>>) where A: Actor;

impl<A> ChildGuard<A> where A: Actor {
    pub(crate) fn new() -> Self {
        ChildGuard(Vec::new())
    }

    pub(crate) fn push<W>(&mut self, mailbox: Mailbox<W>) where A: Supervisor<W>, W: Actor {
        self.0.push(Box::new(mailbox));
    }
}

impl<A> Drop for ChildGuard<A> where A: Actor {
    fn drop(&mut self) {
        for child in &self.0 {
            child.notify_supervisor_stopped();
        }
    }
}

pub(crate) struct SupervisorGuard<A> where A: Actor { id: usize, inner: Arc<AtomicCell<Option<Box<dyn Supervises<A>>>>> }

impl<A> SupervisorGuard<A> where A: Actor {
    pub(crate) fn new(id: usize, sup: Option<Box<dyn Supervises<A>>>) -> Self {
        SupervisorGuard { id, inner: Arc::new(AtomicCell::new(sup)) }
    }

    pub(crate) fn is_some(&self) -> bool {
        let x = self.inner.swap(None);
        let some = x.is_some();
        self.inner.swap(x);
        some
    }

    pub(crate) fn guard(&self) -> Arc<AtomicCell<Option<Box<dyn Supervises<A>>>>> {
        self.inner.clone()
    }
}

impl<A> Drop for SupervisorGuard<A> where A: Actor {
    fn drop(&mut self) {
        println!("Dropping supervisor guard");
        if let Some(sup) = self.inner.swap(None) {
            println!("Notifying supvervisor that worker stopped");
            sup.notify_worker_stopped(self.id, None);
        }
    }
}
