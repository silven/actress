use std::any::TypeId;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use hyper::{Body, HeaderMap, Request, Response, Server};
use hyper::header::{CONTENT_LENGTH};
use hyper::service::{make_service_fn, service_fn};
use serde::{Serialize};
use serde::de::DeserializeOwned;

use crate::{Actor, Handle, Mailbox, Message};
use std::sync::{Weak, RwLock, Arc};
use std::error::Error;


type PBF<T> = Pin<Box<dyn Future<Output = T> + Send + Sync + 'static>>;

pub(crate) trait JsonHandler: Send + Sync + 'static {
    fn handle_json(&self, req: serde_json::Value) -> PBF<Result<serde_json::Value, &'static str>>;
}

fn ff<R, F: Future<Output = R> + Send + Sync + 'static>(fut: F) -> PBF<R> {
    Box::pin(fut)
}

pub(crate) trait JsonMailbox<M>
    where
        M: Message + DeserializeOwned,
        M::Result: Send + Serialize,
{
    fn handle(&self, msg: M) -> PBF<Result<M::Result, &'static str>>;
}

impl<M> JsonHandler for Box<dyn JsonMailbox<M> + Send + Sync + 'static>
    where
        M: Message + DeserializeOwned + Send,
        M::Result: Send + Serialize,
{
    fn handle_json(&self, json: serde_json::Value) -> PBF<Result<serde_json::Value, &'static str>> {
        if let Ok(data) = serde_json::from_value(json) {
            let actor_fut = self.handle(data);
            ff(async move {
                match actor_fut.await {
                    Ok(m) => match serde_json::to_value(m) {
                        Ok(value) => Ok(value),
                        Err(_) => Err("Could not deserialize"),
                    },
                    Err(e) => Err(e),
                }
            })
        } else {
            Box::pin(futures::future::err("Could not deserialize"))
        }
    }
}

impl<A, M> JsonMailbox<M> for Mailbox<A>
    where
        M: Message + DeserializeOwned + Send + Sync,
        M::Result: Send + Sync + Serialize,
        A: Actor + Handle<M>,
{
    fn handle(&self, msg: M) -> PBF<Result<M::Result, &'static str>> {
        println!("Inside AcceptsHttp for {:?}", TypeId::of::<M>());

        if let Ok(reply_fut) = self.ask_nicely(msg) {
            ff(async move {
                match reply_fut.await {
                    Ok(Some(r)) => Ok(r),
                    Ok(None) => Err("Message was dropped"),
                    Err(_) => Err("Could not recv"),
                }
            })
        } else {
            Box::pin(futures::future::err("Could not send"))
        }
    }
}

fn webbox<A, M>(mailbox: Mailbox<A>) -> Box<dyn JsonHandler>
    where
        A: Actor + Handle<M>,
        M: Message + DeserializeOwned + Send + Sync,
        M::Result: Send + Serialize + Sync,
{
    // I don't like this double box thingy
    Box::new(Box::new(mailbox) as Box<dyn JsonMailbox<M> + Send + Sync + 'static>)
}


async fn collect_body(headers: &HeaderMap, mut body: Body) -> Result<Vec<u8>, &'static str> {
    let c_len: usize = headers
        .get(CONTENT_LENGTH)
        .and_then(|x| x.to_str().ok())
        .and_then(|x| x.parse().ok())
        .unwrap_or(128);

    println!("This is the c_len {:?}", c_len);

    let mut buff = Vec::with_capacity(c_len);

    while let Some(next) = body.next().await {
        match next {
            Ok(chunk) => {
                println!("Read a chunk: {:?}", chunk);
                buff.extend_from_slice(chunk.as_ref());
            }
            Err(err) => {
                eprintln!("Body Error: {}", err);
                return Err("body error");
            }
        }
    }
    Ok(buff)
}

pub struct Router {
    routes: RwLock<HashMap<String, Arc<dyn JsonHandler>>>,
}

impl Router {
    pub fn new() -> Self {
        Router { routes: RwLock::new(HashMap::new()) }
    }

    pub(crate) fn serve<M>(&self, path: &str, mailbox: Box<dyn JsonMailbox<M> + Send + Sync + 'static>)
        where M: Message + DeserializeOwned, M::Result: Serialize {
        if let Ok(mut lock) = self.routes.write() {
            lock.insert(path.to_owned(), Arc::new(mailbox));
        }
    }

    fn route(&self, path: &str) -> Option<Arc<dyn JsonHandler>> {
        if let Ok(lock) = self.routes.read() {
            lock.get(path).cloned()
        } else {
            None
        }
    }
}

pub(crate) async fn serve_it(routes: Weak<Router>) {
    {
        let addr = "127.0.0.1:12345".parse().unwrap();

        let mk_service = make_service_fn(move |t| {
            let routes = routes.clone();
            println!("Got a target: {:?}", t);
            async move {
                Ok::<_, hyper::Error>(service_fn(move |req: Request<Body>| {
                    println!("Got a request: {:?}", req);
                    let routes = routes.clone();
                    async move {
                        match serve_request(routes, req).await {
                            Ok(resp) => Ok::<_, hyper::error::Error>(resp),
                            Err(msg) => {
                                let desc = msg.description().to_owned();
                                Ok(Response::builder().status(500).body(Body::from(desc)).unwrap())
                            }
                        }
                    }
                }))
            }
        });

        Server::bind(&addr).serve(mk_service).await;
    }
}

async fn serve_request(routes: Weak<Router>, req: Request<Body>) -> Result<Response<Body>, Box<dyn Error>> {
    let (parts, body) = req.into_parts();
    let body_bytes = collect_body(&parts.headers, body).await?;

    if let Ok(json_value) = serde_json::from_slice(&body_bytes) {
        if let Some(routes) = routes.upgrade() {
            if let Some(handler) = routes.route(parts.uri.path()) {
                let json = handler.handle_json(json_value).await?;
                let body = serde_json::to_string(&json).unwrap();
                return Ok(Response::new(Body::from(body)));
            }
        }
    }
    Ok(Response::builder().status(404).body(Body::from("No route")).unwrap())
}