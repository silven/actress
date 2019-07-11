use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, HeaderMap, Request, Response, Server, StatusCode, Uri};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::mailbox::MailboxAskError;
use crate::{Actor, ActorContext, AsyncResponse, Handle, Mailbox, Message};
use hyper::client::ResponseFuture;
use std::marker::PhantomData;
use hyper::upgrade::Upgraded;
use headers::{HeaderMapExt, Connection, Upgrade, ContentLength};

type PBF<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

pub(crate) trait JsonHandler: Send + 'static {
    fn handle_json(&self, req: serde_json::Value) -> PBF<Result<serde_json::Value, &'static str>>;
}

pub(crate) trait JsonMailbox<M>
where
    M: Message + DeserializeOwned,
    M::Result: Serialize,
{
    fn handle(&self, msg: M) -> PBF<Result<M::Result, &'static str>>;
}

impl<M> JsonHandler for Box<dyn JsonMailbox<M> + Send + Sync + 'static>
where
    M: Message + DeserializeOwned,
    M::Result: Serialize,
{
    fn handle_json(&self, json: serde_json::Value) -> PBF<Result<serde_json::Value, &'static str>> {
        if let Ok(data) = serde_json::from_value(json) {
            let actor_fut = self.handle(data);
            Box::pin(async move {
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
            Box::pin(async move {
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

pub(crate) async fn collect_body(
    headers: &HeaderMap,
    mut body: Body,
) -> Result<Vec<u8>, hyper::Error> {
    // Much hassle to read the content-length header
    let c_len = headers.typed_get::<ContentLength>().map(|c| c.0 as usize);

    let mut buff = Vec::with_capacity(c_len.unwrap_or(512));
    while let Some(next) = body.next().await {
        let chunk = next?;
        buff.extend_from_slice(chunk.as_ref());
    }
    Ok(buff)
}

pub struct Router {
    routes: HashMap<String, Arc<dyn JsonHandler + Send + Sync + 'static>>,
}

impl Router {
    pub fn new() -> Self {
        Router {
            routes: HashMap::new(),
        }
    }
}

impl Actor for Router {}

// Json message

struct JsonMessage(String, serde_json::Value);

enum HyperResponse {
    NotFound,
    Json(serde_json::Value),
}

impl Message for JsonMessage {
    type Result = HyperResponse;
}

impl Handle<JsonMessage> for Router {
    type Response = AsyncResponse<JsonMessage>;

    fn accept(&mut self, msg: JsonMessage, cx: &mut ActorContext<Self>) -> Self::Response {
        //println!("Got request to {} containing {:?}", msg.0, msg.1);
        let handler = self.routes.get(&msg.0).cloned();

        AsyncResponse::from_future(async move {
            if let Some(handler) = handler {
                // TODO: Add a tokio timer/timeout here
                match handler.handle_json(msg.1).await {
                    Ok(reply) => HyperResponse::Json(reply),
                    Err(e) => HyperResponse::Json(serde_json::Value::String(e.to_owned())),
                }
            } else {
                HyperResponse::NotFound
            }
        })
    }
}

// Serve

pub(crate) struct Serve<M>(
    pub(crate) String,
    pub(crate) Box<dyn JsonMailbox<M> + Send + Sync + 'static>,
)
where
    M: Message,
    M::Result: Serialize;

impl<M> Message for Serve<M>
where
    M: Message,
    M::Result: Serialize,
{
    type Result = ();
}

impl<M> Handle<Serve<M>> for Router
where
    M: Message + DeserializeOwned,
    M::Result: Serialize,
{
    type Response = ();

    fn accept(&mut self, msg: Serve<M>, cx: &mut ActorContext<Self>) -> Self::Response {
        self.routes.insert(msg.0, Arc::new(msg.1));
    }
}

// Start hyper thread

pub(crate) fn serve_it(router: Mailbox<Router>) -> impl Future<Output = ()> {
    async move {
        let addr = "127.0.0.1:12345".parse().unwrap();

        let mk_service = make_service_fn(move |_| {
            let router = router.copy();
            async move {
                Ok::<_, hyper::Error>(service_fn(move |req: Request<Body>| {
                    let router = router.copy();
                    async move {
                        if req.headers().typed_get::<Upgrade>().is_some() {
                            println!("upgrade request: {:?}", req);
                            use headers::{SecWebsocketKey, SecWebsocketAccept};

                            let key = req.headers().typed_get::<SecWebsocketKey>().unwrap();
                            let accept = SecWebsocketAccept::from(key.clone());
                            println!("Read key = {:?} derived = {:?}", key, accept);
                            // TODO; Find out how to turn websocket object into channel
                            tokio::spawn(async move {
                                match req.into_body().on_upgrade().await {
                                    Ok(upgraded) => {
                                        println!(
                                            "Got upgraded websocket: {:?}, now what to do with it?",
                                            upgraded
                                        );
                                        use futures::future::poll_fn;
                                        use tokio::prelude::*;

                                        let mut u: Upgraded = upgraded;
                                        //let mut buff = Vec::with_capacity(512);
                                        let msg = "HEEELEOOO";
                                        //poll_fn(|cx| <Upgraded as AsyncRead>::poll_read(Pin::new(&mut u), cx, &mut buff)).await;
                                        let bs = poll_fn(|cx| <Upgraded as AsyncWrite>::poll_write(Pin::new(&mut u), cx, msg.as_bytes())).await;
                                        println!("Wrote some by bytes: {:?}", bs);
                                    }
                                    Err(err) => {
                                        println!("Could not upgrade websocket: {:?}", err);
                                    }
                                }
                            });

                            let mut res = Response::default();

                            *res.status_mut() = StatusCode::SWITCHING_PROTOCOLS;

                            res.headers_mut().typed_insert(Connection::upgrade());
                            res.headers_mut().typed_insert(Upgrade::websocket());
                            res.headers_mut().typed_insert(accept);

                            Ok::<_, hyper::Error>(res)
                        } else {
                            Ok::<_, hyper::Error>(serve_request(router, req).await)
                        }
                    }
                }))
            }
        });
        Server::bind(&addr).serve(mk_service).await;
    }
}

async fn serve_request(router: Mailbox<Router>, req: Request<Body>) -> Response<Body> {
    match deserialize_body(req).await {
        Ok(msg) => match router.ask_async(msg).await {
            Ok(to_resp) => match to_resp {
                HyperResponse::Json(json) => {
                    let as_string = serde_json::to_string(&json).unwrap();
                    Response::builder()
                        .status(StatusCode::OK)
                        .body(Body::from(as_string))
                        .unwrap()
                }
                HyperResponse::NotFound => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from("No route"))
                    .unwrap(),
            },
            Err(err) => {
                eprintln!("Could not route msg: {:?}", err);
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from("Internal Server Error"))
                    .unwrap()
            }
        },
        Err(err) => Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(Body::from(err))
            .unwrap(),
    }
}

// TODO: Why can't this function return a Box<dyn Error>?
async fn deserialize_body(req: Request<Body>) -> Result<JsonMessage, &'static str> {
    let (parts, body) = req.into_parts();
    let body_bytes = collect_body(&parts.headers, body)
        .await
        .or(Err("Could not read body"))?;
    let json_value = serde_json::from_slice(&body_bytes).or(Err("Could not deserialize"))?;
    Ok(JsonMessage(parts.uri.path().to_owned(), json_value))
}

pub struct HttpMailbox<M>
where
    M: Message + Serialize,
    M::Result: DeserializeOwned,
{
    _phantom: PhantomData<*const M>,
    uri: Uri,
    client: hyper::Client<hyper::client::HttpConnector>,
}

unsafe impl<M> Send for HttpMailbox<M>
where
    M: Message + Serialize,
    M::Result: DeserializeOwned,
{
}

impl<M> HttpMailbox<M>
where
    M: Message + Serialize,
    M::Result: DeserializeOwned,
{
    pub fn new_at(path: &str) -> Option<Self> {
        match path.parse::<Uri>() {
            Ok(url) => Some(HttpMailbox::<M> {
                _phantom: PhantomData,
                uri: url,
                client: hyper::Client::new(),
            }),
            _ => None,
        }
    }

    pub fn ask_async(&self, msg: M) -> impl Future<Output = Result<M::Result, MailboxAskError>> {
        let as_json = serde_json::to_string(&msg).unwrap();
        let body = hyper::Body::from(as_json);
        let resp_fut: ResponseFuture = self.client.request(
            Request::post(self.uri.clone())
                .body(body)
                .expect("request builder"),
        );

        async move {
            match resp_fut.await {
                Ok(resp) => {
                    let (parts, body) = resp.into_parts();
                    // TODO; Check HTTP return code
                    match collect_body(&parts.headers, body).await {
                        Ok(bytes) => match serde_json::from_slice(&bytes) {
                            Ok(result) => Ok(result),
                            Err(decode_err) => {
                                let as_str = String::from_utf8_lossy(&bytes);
                                eprintln!(
                                    "Decode err: {:?} str={}, bytes = {:?}",
                                    decode_err, as_str, bytes
                                );
                                Err(MailboxAskError::CouldNotRecv)
                            }
                        },
                        Err(msg) => {
                            eprintln!("Recv err: {:?}", msg);
                            Err(MailboxAskError::CouldNotRecv)
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Client error: {:?}", e);
                    Err(MailboxAskError::CouldNotRecv)
                }
            }
        }
    }
}
