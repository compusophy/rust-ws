#[macro_use]
extern crate rocket;

use rocket::form::Form;
use rocket::http::{Cookie, CookieJar, Status, Header};
use rocket::tokio::sync::broadcast::{channel, Sender};
use rocket::{State, Request, Response};
use rocket::fairing::{Fairing, Info, Kind};
use rocket_dyn_templates::{context, Template};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use rocket_ws::{WebSocket, Message, Channel};
use rocket::serde::json::json;
use futures_util::{SinkExt, StreamExt};
use rocket::fs::{FileServer, relative};

use crate::db::{add_todo, delete_todo, DbError, get_todo, get_todos, maybe_create_database, update_todo};
use serde::Serialize;

mod db;

const DB_URL: &str = "sqlite://sqlite.db";

// Channel capacity for the todo updates
const CHANNEL_CAPACITY: usize = 1024;

// Message struct for broadcasting updates
#[derive(Debug, Clone, Serialize)]
struct TodoUpdate {
    event: String,
    todo_id: Option<i64>,
    source_id: Option<String>,
    content: Option<String>,  // For real-time editing updates
    connected_users: Option<usize>, // For online user count
}

// Track client sessions
#[derive(Default)]
struct ClientSessions(Arc<Mutex<HashSet<String>>>);

impl ClientSessions {
    // Add a client and return the new count
    fn add_client(&self, client_id: &str) -> usize {
        let mut sessions = self.0.lock().unwrap();
        
        // Log if client already exists (shouldn't happen normally)
        if sessions.contains(client_id) {
            println!("WARNING: Client {} already exists in sessions", client_id);
        }
        
        sessions.insert(client_id.to_string());
        sessions.len()
    }
    
    // Remove a client and return the new count
    fn remove_client(&self, client_id: &str) -> usize {
        let mut sessions = self.0.lock().unwrap();
        
        // Log if we're trying to remove a non-existent client
        if !sessions.contains(client_id) {
            println!("WARNING: Trying to remove non-existent client {}", client_id);
        }
        
        sessions.remove(client_id);
        sessions.len()
    }
    
    // Get the current count
    fn count(&self) -> usize {
        let sessions = self.0.lock().unwrap();
        sessions.len()
    }
    
    // Clear all sessions - for debugging purposes
    fn debug_clear(&self) -> usize {
        let mut sessions = self.0.lock().unwrap();
        println!("DEBUG: Clearing all {} sessions", sessions.len());
        sessions.clear();
        0
    }
    
    // Debug print all sessions
    fn debug_print(&self) {
        let sessions = self.0.lock().unwrap();
        println!("DEBUG: Current sessions ({}):", sessions.len());
        for session in sessions.iter() {
            println!("  - {}", session);
        }
    }
}

// Custom fairing to set headers for iframe embedding
pub struct FrameHeaders;

#[rocket::async_trait]
impl Fairing for FrameHeaders {
    fn info(&self) -> Info {
        Info {
            name: "Frame Headers",
            kind: Kind::Response
        }
    }

    async fn on_response<'r>(&self, _request: &'r Request<'_>, response: &mut Response<'r>) {
        // Remove X-Frame-Options header if present
        response.remove_header("X-Frame-Options");
        
        // Add a Content-Security-Policy that allows embedding
        response.set_header(Header::new(
            "Content-Security-Policy", 
            "frame-ancestors 'self' https://*.warpcast.com https://*.farcaster.xyz https://*.fcast.me https://*.farcaster.network https://*.neynar.com *;"
        ));
    }
}

#[rocket::main]
async fn main() -> Result<(), rocket::Error> {
    maybe_create_database().await.expect("Failed to create DB");

    let sessions = ClientSessions::default();

    let _rocket = rocket::build()
        .attach(Template::fairing())
        .attach(FrameHeaders)
        .manage(channel::<TodoUpdate>(CHANNEL_CAPACITY).0)
        .manage(sessions)
        .mount(
            "/",
            routes![
                get_index,
                post_todos,
                get_todo_read,
                get_todo_edit,
                post_todo_edit,
                delete_todo_endpoint,
                todo_websocket
            ],
        )
        .mount("/.well-known", FileServer::from(relative!("static/.well-known")))
        .launch()
        .await?;
    Ok(())
}

// Get or create a unique client ID
fn get_client_id(cookies: &CookieJar<'_>, sessions: &State<ClientSessions>) -> String {
    // Check if client already has an ID
    if let Some(cookie) = cookies.get("client_id") {
        return cookie.value().to_string();
    }
    
    // Generate a new ID
    let new_id = format!("client_{}", rand::random::<u64>());
    
    // Just log the new ID creation, but don't add to active sessions here
    // (This will happen via WebSocket connection)
    println!("New client ID created: {}", new_id);
    
    // Set cookie
    cookies.add(Cookie::new("client_id", new_id.clone()));
    
    new_id
}

#[get("/")]
async fn get_index(cookies: &CookieJar<'_>, sessions: &State<ClientSessions>) -> Result<Template, Status> {
    // Ensure client has an ID
    let _client_id = get_client_id(cookies, sessions);
    
    let todos = get_todos().await?;
    Ok(Template::render(
        "index",
        context! {
            todos
        },
    ))
}

// WebSocket endpoint for real-time updates
#[get("/todo-ws")]
fn todo_websocket<'r>(ws: WebSocket, queue: &'r State<Sender<TodoUpdate>>, sessions: &'r State<ClientSessions>) -> Channel<'r> {
    // Generate a random client ID for this WebSocket connection
    let ws_client_id = format!("ws_client_{}", rand::random::<u64>());
    
    // Debug print current sessions
    sessions.debug_print();
    
    // Create a subscription to the broadcast channel
    let mut rx = queue.subscribe();
    
    // Create the WebSocket channel
    ws.channel(move |mut stream| {
        Box::pin(async move {
            // Add this client to active sessions and get the updated count
            let connected_users = sessions.add_client(&ws_client_id);
            println!("New WebSocket connection: {}. Total connected users: {}", ws_client_id, connected_users);
            
            // Debug print sessions again
            sessions.debug_print();
            
            // Broadcast user count to all clients
            let _ = queue.send(TodoUpdate {
                event: "user_count".to_string(),
                todo_id: None,
                source_id: None,
                content: None,
                connected_users: Some(connected_users),
            });
            
            // First, try to send the initial list of todos
            if let Ok(todos) = get_todos().await {
                let initial_msg = json!({
                    "event": "init",
                    "todos": todos,
                    "connected_users": connected_users
                });
                
                if let Ok(json_str) = serde_json::to_string(&initial_msg) {
                    let _ = stream.send(Message::Text(json_str)).await;
                }
            }
            
            // Create a loop to handle both WebSocket messages and broadcast channel messages
            loop {
                rocket::tokio::select! {
                    // Handle broadcasts from the queue
                    msg = rx.recv() => {
                        if let Ok(update) = msg {
                            // Skip messages from this client by checking source_id
                            if let Some(source_id) = &update.source_id {
                                // Debugging to see what's happening
                                println!("WS received update: {:?}, ws_client_id: {}", update, ws_client_id);
                                
                                // Match our WebSocket client ID with the cookieJar client ID
                                if source_id.starts_with("client_") {
                                    // Extract the cookie's value and send it to client for verification
                                    let info_msg = json!({
                                        "event": "debug_info",
                                        "your_ws_id": ws_client_id,
                                        "source_id": source_id,
                                    });
                                    if let Ok(info_str) = serde_json::to_string(&info_msg) {
                                        let _ = stream.send(Message::Text(info_str)).await;
                                    }
                                }
                            }
                            
                            // Just forward the JSON representation of the update
                            if let Ok(json_str) = serde_json::to_string(&update) {
                                if stream.send(Message::Text(json_str)).await.is_err() {
                                    break;
                                }
                            }
                        } else {
                            // Error receiving broadcast message
                            break;
                        }
                    },
                    
                    // Handle incoming messages from WebSocket
                    msg = stream.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                println!("Received message from client: {}", text);
                                // Try to parse as JSON
                                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                                    // If this is a client ID message, store it
                                    if let Some(client_id) = value.get("client_id") {
                                        if let Some(id_str) = client_id.as_str() {
                                            println!("WebSocket client {} identified as {}", ws_client_id, id_str);
                                        }
                                    }
                                    
                                    // If this is a real-time edit update
                                    if let Some(event) = value.get("event") {
                                        if event.as_str() == Some("edit_update") {
                                            if let (Some(todo_id), Some(content)) = (
                                                value.get("todo_id").and_then(|v| v.as_i64()),
                                                value.get("content").and_then(|v| v.as_str())
                                            ) {
                                                // Get client ID from message if available
                                                let source_id = value.get("client_id")
                                                    .and_then(|v| v.as_str())
                                                    .map(|s| s.to_string())
                                                    .unwrap_or_else(|| ws_client_id.clone());
                                                
                                                // Broadcast the edit to all clients
                                                let _ = queue.send(TodoUpdate {
                                                    event: "edit_update".to_string(),
                                                    todo_id: Some(todo_id),
                                                    source_id: Some(source_id),
                                                    content: Some(content.to_string()),
                                                    connected_users: None,
                                                });
                                            }
                                        }
                                        
                                        // If this is a save edit
                                        if event.as_str() == Some("save_edit") {
                                            if let (Some(todo_id), Some(content)) = (
                                                value.get("todo_id").and_then(|v| v.as_i64()),
                                                value.get("content").and_then(|v| v.as_str())
                                            ) {
                                                // Get client ID from message if available
                                                let source_id = value.get("client_id")
                                                    .and_then(|v| v.as_str())
                                                    .map(|s| s.to_string())
                                                    .unwrap_or_else(|| ws_client_id.clone());
                                                
                                                // Actually save the edit to the database
                                                if let Ok(_) = update_todo(todo_id, &content.to_string()).await {
                                                    println!("Saved edit for todo {}: {}", todo_id, content);
                                                    
                                                    // Send confirmation back to client
                                                    let confirm_msg = json!({
                                                        "event": "edit_saved",
                                                        "todo_id": todo_id,
                                                        "success": true
                                                    });
                                                    
                                                    if let Ok(confirm_str) = serde_json::to_string(&confirm_msg) {
                                                        let _ = stream.send(Message::Text(confirm_str)).await;
                                                    }
                                                    
                                                    // Broadcast final update to all clients
                                                    let _ = queue.send(TodoUpdate {
                                                        event: "update".to_string(),
                                                        todo_id: Some(todo_id),
                                                        source_id: Some(source_id),
                                                        content: Some(content.to_string()),
                                                        connected_users: None,
                                                    });
                                                }
                                            }
                                        }
                                    }
                                }
                            },
                            Some(Ok(Message::Close(_))) => {
                                // Remove this client from active sessions and get updated count
                                let connected_users = sessions.remove_client(&ws_client_id);
                                println!("WebSocket connection closed: {}. Total connected users: {}", ws_client_id, connected_users);
                                
                                // Debug print sessions after disconnect
                                sessions.debug_print();
                                
                                // Broadcast user count update
                                let _ = queue.send(TodoUpdate {
                                    event: "user_count".to_string(),
                                    todo_id: None,
                                    source_id: None,
                                    content: None,
                                    connected_users: Some(connected_users),
                                });
                                
                                break;
                            },
                            None => {
                                // Remove this client from active sessions and get updated count
                                let connected_users = sessions.remove_client(&ws_client_id);
                                println!("WebSocket connection lost: {}. Total connected users: {}", ws_client_id, connected_users);
                                
                                // Debug print sessions after disconnect
                                sessions.debug_print();
                                
                                // Broadcast user count update
                                let _ = queue.send(TodoUpdate {
                                    event: "user_count".to_string(),
                                    todo_id: None,
                                    source_id: None,
                                    content: None,
                                    connected_users: Some(connected_users),
                                });
                                
                                break;
                            }
                            _ => {}
                        }
                    }
                }
            }
            
            Ok(())
        })
    })
}

#[derive(FromForm)]
struct TodoForm {
    title: String,
}

#[post("/todos", data = "<form>")]
async fn post_todos(cookies: &CookieJar<'_>, sessions: &State<ClientSessions>, form: Form<TodoForm>, queue: &State<Sender<TodoUpdate>>) -> String {
    let client_id = get_client_id(cookies, sessions);
    let id = add_todo(&form.title).await.unwrap_or(-1);
    
    // Return error if adding failed
    if id == -1 {
        return "Error adding todo".to_string();
    }
    
    println!("✅ Created new todo with id: {}", id);
    
    // Broadcast the new todo to all clients, but don't include user count
    let _ = queue.send(TodoUpdate {
        event: "add".to_string(),
        todo_id: Some(id),
        source_id: Some(client_id),  // Include source_id to identify source
        content: None,
        connected_users: None,  // Don't send connected users here
    });
    
    // Just return the ID as a simple string
    id.to_string()
}

#[post("/todo-edit/<id>", data = "<form>")]
async fn post_todo_edit(cookies: &CookieJar<'_>, sessions: &State<ClientSessions>, id: i64, form: Form<TodoForm>, queue: &State<Sender<TodoUpdate>>) -> Result<Template, Status> {
    let client_id = get_client_id(cookies, sessions);
    update_todo(id, &form.title).await?;
    let todo = get_todo(id).await?;
    
    // Broadcast update to all clients, but don't include user count
    let _ = queue.send(TodoUpdate {
        event: "update".to_string(),
        todo_id: Some(id),
        source_id: Some(client_id),
        content: None,
        connected_users: None,  // Don't send connected users here
    });
    
    Ok(Template::render(
        "todo-read",
        context! {
            todo
        },
    ))
}

#[get("/todo-edit/<id>")]
async fn get_todo_edit(id: i64) -> Result<Template, Status> {
    let todo = get_todo(id).await?;
    Ok(Template::render(
        "todo-read",
        context! {
            todo,
            edit_mode: true
        },
    ))
}

#[get("/todo-read/<id>")]
async fn get_todo_read(id: i64) -> Result<Template, Status> {
    let todo = get_todo(id).await?;
    println!("GET todo_read for id {}", id);
    Ok(Template::render(
        "todo-read",
        context! {
            todo
        },
    ))
}

// Add a new endpoint to delete a specific todo
#[post("/todo-delete/<id>")]
async fn delete_todo_endpoint(cookies: &CookieJar<'_>, sessions: &State<ClientSessions>, id: i64, queue: &State<Sender<TodoUpdate>>) -> Status {
    let client_id = get_client_id(cookies, sessions);
    
    // Delete the todo
    if let Err(_) = delete_todo(id).await {
        return Status::InternalServerError;
    }
    
    // Broadcast delete event to all clients, but don't include user count
    let _ = queue.send(TodoUpdate {
        event: "delete".to_string(),
        todo_id: Some(id),
        source_id: Some(client_id),
        content: None,
        connected_users: None,  // Don't send connected users here
    });
    
    Status::Ok
}

impl From<DbError> for Status {
    fn from(_: DbError) -> Self {
        Status::InternalServerError
    }
}

