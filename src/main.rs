use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use futures::{SinkExt, StreamExt, TryFutureExt};
use tokio::sync::{mpsc, RwLock};
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::ws::{Message, WebSocket};
use warp::Filter;

type UID = usize;
type Users = Arc<RwLock<HashMap<UID, mpsc::UnboundedSender<Message>>>>;

static USER_ID_GEN: AtomicUsize = AtomicUsize::new(1);

async fn on_user_connected(ws: WebSocket, users: Users) {
    let local_id = USER_ID_GEN.fetch_add(1, Ordering::Relaxed);

    let (mut user_ws_tx, mut user_ws_rx) = ws.split();

    // Use the tx & rx queues as unbounded in-memory buffers for
    // queueing, buffering, and flushing messages.
    let (tx, rx) = mpsc::unbounded_channel();
    let mut rx = UnboundedReceiverStream::new(rx);

    tokio::task::spawn(async move {
        while let Some(message) = rx.next().await {
            user_ws_tx
                .send(message)
                .unwrap_or_else(|e| {
                    eprintln!("Error reading msg: {}", e);
                })
                .await;
        }
    });

    // Create a new scope to release the write lock
    // after inserting the new ID.
    {
        let mut users_write_mtx = users.write().await;
        users_write_mtx.insert(local_id, tx);
    }

    // Handle new messages in the client writer queue and process
    // them.
    while let Some(result) = user_ws_rx.next().await {
        let msg = match result {
            Ok(msg) => msg,
            Err(e) => {
                eprintln!("websocket error(uid={}): {}", local_id, e);
                break;
            }
        };
        handle_user_message(local_id, msg, &users).await;
    }
    user_disconnected(local_id, &users).await;
}

async fn user_disconnected(local_id: usize, users: &Users) {
    eprintln!("good bye user: {}", local_id);

    // Stream closed up, so remove from the user list
    users.write().await.remove(&local_id);
}

async fn handle_user_message(sender_id: UID, message: Message, users: &Users) {
    eprintln!("Processing user msg {:?}", message);
    let message_str = if let Ok(s) = message.to_str() {
        s
    } else {
        eprintln!("Returning early! {:?}", message);
        return;
    };

    let formatted_text = format!("User {} says: {}", sender_id, message_str);

    eprintln!("sending msg to us: {}", users.read().await.len());

    for (&uid, tx) in users.read().await.iter() {
        println!("Sending message to UID {}", uid);
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

    let chat = warp::path("chat")
        .and(warp::ws())
        .and(users)
        .map(|ws: warp::ws::Ws, users| {
            ws.on_upgrade(move |socket| on_user_connected(socket, users))
        });

    let index = warp::path::end().map(|| warp::reply::html(INDEX_HTML));

    let all_routes = index.or(chat);

    warp::serve(all_routes).run(([127, 0, 0, 1], 8000)).await;
}

static INDEX_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
    <head>
        <title>Warp Chat</title>
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
