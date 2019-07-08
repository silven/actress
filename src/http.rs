use std::collections::HashMap;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock, Weak};

use hyper::{Body, HeaderMap, Request, Response, Server};
use hyper::header::CONTENT_LENGTH;
use hyper::service::{make_service_fn, service_fn};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{Actor, Handle, Mailbox, Message};
use std::thread::JoinHandle;

type PBF<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

pub(crate) trait JsonHandler: 'static {
    fn handle_json(&self, req: serde_json::Value) -> PBF<Result<serde_json::Value, &'static str>>;
}

fn ff<R, F: Future<Output = R> + 'static>(fut: F) -> PBF<R> {
    Box::pin(fut)
}

pub(crate) trait JsonMailbox<M>
    where
        M: Message + DeserializeOwned,
        M::Result: Serialize,
{
    fn handle(&self, msg: M) -> PBF<Result<M::Result, &'static str>>;
}

impl<M> JsonHandler for Box<dyn JsonMailbox<M> + 'static>
    where
        M: Message + DeserializeOwned,
        M::Result: Serialize,
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
        M: Message + DeserializeOwned,
        M::Result: Serialize,
        A: Actor + Handle<M>,
{
    fn handle(&self, msg: M) -> PBF<Result<M::Result, &'static str>> {
        if let Ok(reply_fut) = self.ask_future(msg) {
            ff(async move {
                match reply_fut.await {
                    Ok(Some(r)) => Ok(r),
                    Ok(None) => Err("Message was dropped"),
                    Err(_) => Err("Could not recv reply"),
                }
            })
        } else {
            Box::pin(futures::future::err("Could not send request"))
        }
    }
}

async fn collect_body(headers: &HeaderMap, mut body: Body) -> Result<Vec<u8>, hyper::Error> {
    // Much hassle to read the content-length header
    let c_len: usize = headers
        .get(CONTENT_LENGTH)
        .and_then(|x| x.to_str().ok())
        .and_then(|x| x.parse().ok())
        .unwrap_or(128);

    let mut buff = Vec::with_capacity(c_len);
    while let Some(next) = body.next().await {
        let chunk = next?;
        buff.extend_from_slice(chunk.as_ref());
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

    pub(crate) fn serve<M>(&self, path: &str, mailbox: Box<dyn JsonMailbox<M> + 'static>)
        where M: Message + DeserializeOwned,
              M::Result: Serialize {
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

pub(crate) fn serve_it(router: Weak<Router>) -> std::thread::JoinHandle<()> {
    let hyper_thread: JoinHandle<()> = std::thread::spawn(|| {
        hyper::rt::run(async move {
            let addr = "127.0.0.1:12345".parse().unwrap();

            let mk_service = make_service_fn(move |_| {
                let router = router.clone();
                async move {
                    Ok::<_, hyper::Error>(service_fn(move |req: Request<Body>| {
                        let router = router.clone();
                        async move {
                            //Ok::<_, hyper::Error>(Response::builder().status(200).body(Body::from("hi!".to_owned())).unwrap())

                            match serve_request(router, req).await {
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
        });
    });

    hyper_thread
}

async fn serve_request(routes: Weak<Router>, req: Request<Body>) -> Result<Response<Body>, Box<dyn Error>> {
    let (parts, body) = req.into_parts();
    let body_bytes = collect_body(&parts.headers, body).await?;

    let json_value = serde_json::from_slice(&body_bytes)?;

    let routes = routes.upgrade().ok_or("no routing table")?;

    let handler = routes.route(parts.uri.path()).ok_or("no route")?;

    let json = handler.handle_json(json_value).await?;

    let body = serde_json::to_string(&json)?;

    Ok(Response::new(Body::from(body)))
}