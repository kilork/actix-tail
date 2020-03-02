#[macro_use]
extern crate actix_web;

use actix::prelude::*;
use actix_web::{middleware, web, App, Error, HttpResponse, HttpServer, Responder};
use bytes::Bytes;
use exitfailure::ExitFailure;
use log::debug;
use std::{
    pin::Pin,
    process::Stdio,
    sync::RwLock,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
};

const INDEX_HTML: &str = include_str!("../web/index.html");

#[get("/")]
async fn index() -> impl Responder {
    HttpResponse::Ok()
        .content_type("text/html")
        .body(INDEX_HTML)
}

#[get("/stream")]
async fn stream(broadcaster: web::Data<RwLock<Broadcaster>>) -> impl Responder {
    let rx = broadcaster.write().unwrap().new_client();
    HttpResponse::Ok()
        .content_type("text/event-stream")
        .keep_alive()
        .no_chunking()
        .streaming(rx)
}

#[actix_rt::main]
async fn main() -> Result<(), ExitFailure> {
    env_logger::init();

    let broadcaster = web::Data::new(RwLock::new(Broadcaster::new()));
    let broadcaster_timer = broadcaster.clone();

    let mut cmd = Command::new("cat");
    cmd.stdout(Stdio::piped());
    let mut child = cmd.spawn().expect("failed to spawn command");

    let stdout = child
        .stdout
        .take()
        .expect("child did not have a handle to stdout");

    let mut reader = BufReader::new(stdout).lines();

    let log_task = async move {
        while let Ok(Some(line)) = reader.next_line().await {
            debug!("Line: {}", line);
            let mut me = broadcaster_timer.write().unwrap();

            if let Err(ok_clients) = me.message(&line) {
                debug!("refresh client list");
                me.clients = ok_clients;
            }
        }
    };
    let status_task = async move {
        let status = child.await.expect("child process encountered an error");

        debug!("child status was: {}", status);
    };
    Arbiter::spawn(log_task);
    Arbiter::spawn(status_task);

    HttpServer::new(move || {
        App::new()
            .wrap(middleware::Logger::default())
            .app_data(broadcaster.clone())
            .service(index)
            .service(stream)
    })
    .bind("localhost:8080")?
    .run()
    .await?;

    Ok(())
}

struct Broadcaster {
    clients: Vec<Sender<Bytes>>,
}

impl Broadcaster {
    fn new() -> Self {
        Self { clients: vec![] }
    }

    fn new_client(&mut self) -> Client {
        debug!("adding new client");
        let (tx, rx) = mpsc::channel(100);

        self.clients.push(tx);

        Client(rx)
    }

    fn message(&mut self, msg: &str) -> Result<(), Vec<Sender<Bytes>>> {
        let mut ok_clients = vec![];

        debug!("message to {} client(s)", self.clients.len());

        let msg = Bytes::from(["data: ", msg, "\n\n"].concat());

        for client in &mut self.clients {
            if let Ok(()) = client.try_send(msg.clone()) {
                ok_clients.push(client.clone())
            }
        }

        if ok_clients.len() != self.clients.len() {
            return Err(ok_clients);
        }

        Ok(())
    }
}

// wrap Receiver in own type, with correct error type
struct Client(Receiver<Bytes>);

impl Stream for Client {
    type Item = Result<Bytes, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.0).poll_next(cx) {
            Poll::Ready(Some(v)) => Poll::Ready(Some(Ok(v))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
