use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use futures::{SinkExt, StreamExt, TryFutureExt};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::reply::Html;
use warp::ws::{Message, WebSocket};
use warp::Filter;

type UID = usize;
type Users = Arc<RwLock<HashMap<UID, mpsc::UnboundedSender<Message>>>>;

type HistoryBuffer = Arc<RwLock<Vec<Message>>>;

static USER_ID_GEN: AtomicUsize = AtomicUsize::new(1);

async fn on_user_connected(ws: WebSocket, users: Users) {
    let local_id = USER_ID_GEN.fetch_add(1, Ordering::Relaxed);

    let (mut client_write_queue, mut client_read_queue) = ws.split();

    // Use the tx & rx queues as unbounded in-memory buffers for
    // queueing, buffering, and flushing messages.
    let (tx, rx) = mpsc::unbounded_channel();

    // rx here buffers the inter-thread channel from above.
    // New messages published to rx are messages from other
    // client thread handlers.
    let mut rx = UnboundedReceiverStream::new(rx);

    // Create a new scope to release the write lock
    // after inserting the new ID.
    {
        let mut users_write_mtx = users.write().await;
        users_write_mtx.insert(local_id, tx);
    }

    // Read the inter-thread channel and send other clients'
    // messages through the write queue.
    tokio::task::spawn(async move {
        while let Some(message) = rx.next().await {
            client_write_queue
                .send(message)
                .unwrap_or_else(|e| {
                    eprintln!("Error reading msg: {}", e);
                })
                .await;
        }
    });

    // Handle new messages in the client writer queue and process
    // them. The loop breaks when the client disconnects.
    while let Some(result) = client_read_queue.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("websocket error(uid={}): {}", local_id, e);
                break;
            }
        };
        handle_user_message(local_id, msg, &users).await;
    }
    // Handle disconnect process & clean up resources.
    handle_user_disconnect(local_id, &users).await;
}

async fn handle_user_disconnect(local_id: usize, users: &Users) {
    println!("good bye user: {}", local_id);

    // Stream closed up, so remove from the user list to prevent a
    // hanging write queue.
    users.write().await.remove(&local_id);
}

async fn handle_user_message(sender_id: UID, message: Message, users: &Users) {
    let message_str = if let Ok(message_str) = message.to_str() {
        message_str
    } else {
        println!("Returning early! {:?}", message);
        return;
    };

    let formatted_text = format!("<uid {}> says: {}", sender_id, message_str);

    for (&uid, tx) in users.read().await.iter() {
        if sender_id != uid {
            if let Err(_disconnected) = tx.send(Message::text(formatted_text.clone())) {
                eprintln!("err sending: {:?}", _disconnected);
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let users = Users::default();
    let users = warp::any().map(move || users.clone());

    let history_buffer = HistoryBuffer::default();
    let history_buffer = warp::any().map(move || history_buffer.clone());

    let chat = warp::path("chat")
        .and(warp::ws())
        .and(users)
        .map(|ws: warp::ws::Ws, users| {
            ws.on_upgrade(move |socket| on_user_connected(socket, users))
        });

    let history = warp::path("history")
        .and(history_buffer)
        .map(|history_buffer: HistoryBuffer| {
            let mut messages: Vec<Message> = vec![];

            let mut history_read_mutex = history_buffer.try_read();
            while history_buffer.try_read().is_err() {
                history_read_mutex = history_buffer.try_read();
            }

            for message in history_read_mutex.unwrap().iter() {
                messages.push(message.clone());
            }

            warp::reply::html(format!(
                r#"<!DOCTYPE html>
    <html lang="en">
        <head>
            <title>Basic Rust Warp Chat</title>
        </head>
        <body>
            <h1>
                History!
            </h1>
            {}
        </body>
    </html>
    "#,
                "abcd"
            ))
        });
    let index = warp::path::end().map(|| warp::reply::html(INDEX_HTML));
    let all_routes = index.or(history).or(chat);

    warp::serve(all_routes).run(([127, 0, 0, 1], 8000)).await;
}

static INDEX_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
    <head>
        <title>Basic Rust Warp Chat</title>
        <style>
            button { 
                background-color: white;
                box-shadow: 5px 10px;
                border-radius: 5px;
            }
        </style>
    </head>
    <body>
        <h1>Warp chat</h1>
        <div id="chat">
            <p><em>Connecting...</em></p>
        </div>
        <input type="text" id="text" />
        <button type="button" id="send">Send</button>
        <script type="text/javascript">
            const chat = document.getElementById('chat');
            const text = document.getElementById('text');
            const uri = 'ws://' + location.host + '/chat';
            const ws = new WebSocket(uri);
            function message(data) {
                const line = document.createElement('p');
                line.innerText = data;
                chat.appendChild(line);
            }
            ws.onopen = function() {
                chat.innerHTML = '<p><em>Connected!</em></p>';
            };
            ws.onmessage = function(msg) {
                console.log("Got new data from ws");
                message(msg.data);
            };
            ws.onclose = function() {
                chat.getElementsByTagName('em')[0].innerText = 'Disconnected!';
                document.getElementById("send").disabled = true;
            };
            send.onclick = function() {
                const msg = text.value;
                ws.send(msg);
                text.value = '';
                message('<You>: ' + msg);
            };
        </script>
    </body>
</html>
"#;
