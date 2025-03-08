use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use warp::Filter;
use futures_util::stream::StreamExt;
use futures_util::SinkExt;
use serde::Serialize;
use serde_json::Value;
use log::{info, warn, error};

#[derive(Debug, Clone)]
struct User {
    id: usize,
    tx: mpsc::UnboundedSender<Message>,
}

#[derive(Clone)]
struct WebSocketServer {
    users: Arc<Mutex<HashMap<usize, User>>>,
    next_id: Arc<Mutex<usize>>,
}

impl WebSocketServer {
    fn new() -> Self {
        Self {
            users: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(Mutex::new(0)),
        }
    }

    async fn broadcast(&self, message: impl Serialize) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting broadcast");
        let json = serde_json::to_string(&message)?;
        info!("Acquiring users lock");
        let users = self.users.lock().await;
        info!("Broadcasting to {} users", users.len());
        for user in users.values() {
            info!("Sending message to user {}", user.id);
            if user.tx.send(Message::Text(json.clone())).is_err() {
                warn!("Failed to send message to user {}", user.id);
            }
            info!("Message sent to user {}", user.id);
        }
        info!("Broadcast completed");
        Ok(())
    }

    async fn send_to(&self, user_id: usize, message: impl Serialize) -> Result<(), Box<dyn std::error::Error>> {
        info!("Sending message to user {}", user_id);
        let json = serde_json::to_string(&message)?;
        info!("Acquiring users lock");
        let users = self.users.lock().await;
        if let Some(user) = users.get(&user_id) {
            info!("Sending message to user {}", user_id);
            if user.tx.send(Message::Text(json)).is_err() {
                warn!("Failed to send message to user {}", user_id);
            }
            info!("Message sent to user {}", user_id);
        } else {
            warn!("User {} not found", user_id);
        }
        Ok(())
    }
}

async fn handle_webhook(body: Value, ws_server: WebSocketServer) -> Result<impl warp::Reply, warp::Rejection> {
    // Properly handle the Result returned by broadcast
    if let Err(e) = ws_server.broadcast(body).await {
        error!("Error broadcasting message: {}", e);
        return Ok(warp::reply::with_status("Error broadcasting message", warp::http::StatusCode::INTERNAL_SERVER_ERROR));
    }
    Ok(warp::reply::with_status("Message broadcasted", warp::http::StatusCode::OK))
}

#[tokio::main]
async fn main() {
    // Initialize the logger with a more explicit configuration
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    info!("Starting WebSocket server");
    let ws_server = WebSocketServer::new();
    let ws_server_clone = ws_server.clone();
    let ws_route = warp::path("ws")
        .and(warp::ws())
        .map(move |ws: warp::ws::Ws| {
            let ws_server = ws_server.clone();
            ws.on_upgrade(move |socket| {
                let ws_server = ws_server.clone();
                async move {
                    let id = {
                        let mut next_id = ws_server.next_id.lock().await;
                        *next_id += 1;
                        *next_id
                    };

                    let (tx, mut rx) = mpsc::unbounded_channel();
                    ws_server.users.lock().await.insert(id, User { id, tx });

                    info!("New WebSocket connection: {}", id);
                    let (mut ws_sender, mut ws_receiver) = socket.split();

                    tokio::spawn(async move {
                        while let Some(Ok(msg)) = ws_receiver.next().await {
                            if msg.is_text() {
                                if let Ok(text) = msg.to_str() {
                                    let _ = ws_server.broadcast(text).await;
                                }
                            }
                        }
                        info!("WebSocket connection closed: {}", id);
                        ws_server.users.lock().await.remove(&id);
                    });

                    while let Some(msg) = rx.recv().await {
                        if ws_sender.send(warp::ws::Message::text(msg.to_string())).await.is_err() {
                            break;
                        }
                    }
                }
            })
        });

    let webhook_route = warp::post()
        .and(warp::path("webhook"))
        .and(warp::body::json())
        .and(warp::any().map(move || ws_server_clone.clone()))
        .and_then(handle_webhook);

    let cors = warp::cors()
        .allow_any_origin()
        .allow_methods(vec!["GET", "POST", "OPTIONS"])
        .allow_headers(vec!["Content-Type", "Authorization"])
        .max_age(3600);

    // Combine all routes first, then apply CORS
    let routes = ws_route
        .or(warp::fs::dir("public"))
        .or(webhook_route)
        .with(cors);

    info!("Server running on ws://127.0.0.1:3030");
    warp::serve(routes).run(([127, 0, 0, 1], 3030)).await;
}