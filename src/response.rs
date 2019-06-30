use crate::actor::Message;

use tokio_sync::oneshot;
use tokio_threadpool::Sender;
use std::pin::Pin;
use std::future::Future;
use std::marker::PhantomData;

pub trait Response<M: Message> {
    fn handle(self, spawner: Sender, reply_to: Option<oneshot::Sender<Option<M::Result>>>);
}

pub struct SyncResponse<M:Message>(pub M::Result);
pub struct AsyncResponse<M:Message>(pub Pin<Box<dyn Future<Output = M::Result> + Send>>);
pub struct NoResponse<M: Message>(PhantomData<M>);

impl<M: Message> From<Pin<Box<dyn Future<Output = M::Result> + Send>>> for AsyncResponse<M> {
    fn from(value: Pin<Box<dyn Future<Output = M::Result> + Send>>) -> AsyncResponse<M> {
        AsyncResponse(value)
    }
}

impl<M: Message> From<()> for NoResponse<M> {
    fn from(value: ()) -> NoResponse<M> {
        NoResponse(PhantomData)
    }
}

impl<M> Response<M> for NoResponse<M>
    where
        M: Message,
{
    fn handle(self, _: Sender, _: Option<oneshot::Sender<Option<M::Result>>>) {
        //...
    }
}

impl<M> Response<M> for AsyncResponse<M>
    where
        M: Message,
        M::Result: Send,
{
    fn handle(self, spawner: Sender, reply_to: Option<oneshot::Sender<Option<M::Result>>>) {
        spawner.spawn(async move {
            let result: <M as Message>::Result = self.0.await;
            if let Some(tx) = reply_to {
                tx.send(Some(result));
            }
        });
    }
}

impl<M> Response<M> for SyncResponse<M>
    where
        M: Message,
{
    fn handle(self, _: Sender, reply_to: Option<oneshot::Sender<Option<M::Result>>>) {
        if let Some(tx) = reply_to {
            tx.send(Some(self.0));
        }
    }
}

macro_rules! simple_result {
    ($type:ty) => {
        impl<M: Message> Response<M> for $type
        where
            M: Message<Result = $type>,
        {
            fn handle(self, _: Sender, reply_to: Option<oneshot::Sender<Option<M::Result>>>) {
                if let Some(tx) = reply_to {
                    tx.send(Some(self));
                }
            }
        }
    };
}

//Unfortunate that we can't blanket impl these
simple_result!(());

simple_result!(u8);
simple_result!(u16);
simple_result!(u32);
simple_result!(u64);

simple_result!(i8);
simple_result!(i16);
simple_result!(i32);
simple_result!(i64);

simple_result!(isize);
simple_result!(usize);

// Hmm, this is bad.
simple_result!(String);

