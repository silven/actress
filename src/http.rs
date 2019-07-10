use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use hyper::header::{HeaderValue, CONTENT_LENGTH, UPGRADE};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, HeaderMap, Request, Response, Server, StatusCode, Uri};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{Actor, ActorContext, AsyncResponse, Handle, Mailbox, Message};
use crate::mailbox::MailboxAskError;
use hyper::client::ResponseFuture;
use std::marker::PhantomData;

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

pub(crate) async fn collect_body(headers: &HeaderMap, mut body: Body) -> Result<Vec<u8>, hyper::Error> {
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

pub(crate) fn serve_it(router: Mailbox<Router>) -> impl Future<Output=()> {
    async move {
        let addr = "127.0.0.1:12345".parse().unwrap();

        let mk_service = make_service_fn(move |_| {
            let router = router.copy();
            async move {
                Ok::<_, hyper::Error>(service_fn(move |req: Request<Body>| {
                    let router = router.copy();
                    async move {
                        if req.headers().contains_key(UPGRADE) {
                            // TODO; Find out how to turn websocket object into channel
                            tokio::spawn(async move {
                                match req.into_body().on_upgrade().await {
                                    Ok(upgraded) => {
                                        println!("Got upgraded websocket: {:?}, now what to do with it?", upgraded);
                                    }
                                    Err(err) => {
                                        println!("Could not upgrade websocket: {:?}", err);
                                    }
                                }
                            });

                            Ok::<_, hyper::Error>(
                                Response::builder()
                                    .status(StatusCode::SWITCHING_PROTOCOLS)
                                    .header(UPGRADE, HeaderValue::from_static("websocket"))
                                    .body(Body::empty())
                                    .unwrap(),
                            )
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
            Err(_) => Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("Internal Server Error"))
                .unwrap(),
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



pub struct HttpMailbox<A>
    where
        A: Actor,
{
    _phantom: PhantomData<*const A>,
    uri: Uri,
    client: hyper::Client<hyper::client::HttpConnector>,
}

unsafe impl<A> Send for HttpMailbox<A> where A: Actor {}

impl<A> HttpMailbox<A> where A: Actor {
    pub fn new_at(path: &str) -> Option<Self> {
        match path.parse::<Uri>() {
            Ok(url) => {
                Some(HttpMailbox::<A> {
                    _phantom: PhantomData,
                    uri: url,
                    client: hyper::Client::new()
                })
            },
            _ => None,
        }
    }

    pub fn ask_async<M>(&self, msg: M) -> impl Future<Output=Result<M::Result, MailboxAskError>>
        where
            A: Actor + Handle<M>,
            M: Message + Serialize,
            M::Result: DeserializeOwned,
    {
        let as_json = serde_json::to_string(&msg).unwrap();
        println!("Sending json: {:?}", as_json);
        let body = hyper::Body::from(as_json);
        let resp_fut: ResponseFuture = self.client.request(Request::post(self.uri.clone()).body(body).expect("request builder"));

        async move {
            println!("Inside async");
            let respr: Result<hyper::Response<Body>, hyper::Error> = resp_fut.await;
            match respr {
                Ok(resp) => {
                    println!("inside resp");
                    let (parts, body) = resp.into_parts();
                    let resp_body = collect_body(&parts.headers, body);
                    let resp_result: Result<Vec<u8>, hyper::Error> = resp_body.await;
                    println!("The byte result: {:?}", resp_result);
                    match resp_result {
                        Ok(bytes) => {
                            let as_result = serde_json::from_slice(&bytes).unwrap();
                            Ok(as_result)
                        },
                        Err(msg) => {
                            eprintln!("Recv err: {:?}", msg);
                            Err(MailboxAskError::CouldNotRecv)
                        }
                    }
                },
                Err(e) => {
                    eprintln!("Client error: {:?}", e);
                    Err(MailboxAskError::CouldNotRecv)
                }
            }
        }
    }
}
