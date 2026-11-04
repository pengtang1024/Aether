use crate::server::*;

pub(crate) struct Command {
    pub(crate) request: Request,
    pub(crate) respond: Sender<Response>,
    pub(crate) subscriber: Option<Sender<serde_json::Value>>,
    /// Fired after the response bytes are written, so `shutdown` can wait for
    /// the client to receive its ack before the process exits.
    pub(crate) written: Option<mpsc::Receiver<()>>,
}

/// Subscribe a compatibility transport (aria2 WebSocket) to the same event
/// stream the native `subscribe` method uses.
pub(crate) fn subscribe_events(
    commands: &Sender<Command>,
) -> Option<mpsc::Receiver<serde_json::Value>> {
    let (respond_tx, respond_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let request = Request {
        id: 0,
        method: "subscribe".into(),
        url: None,
        plugin: None,
        dir: None,
        task_id: None,
        priority: None,
        settings: serde_json::Value::Null,
    };
    commands
        .send(Command {
            request,
            respond: respond_tx,
            subscriber: Some(event_tx),
            written: None,
        })
        .ok()?;
    match respond_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(response) if response.ok => Some(event_rx),
        _ => None,
    }
}

pub(crate) fn dispatch(state: &mut State, mut command: Command) -> bool {
    let id = command.request.id;
    let method = command.request.method.clone();
    if method == "subscribe" {
        if let Some(tx) = command.subscriber {
            state.subscribers.push(tx);
        }
        let _ = command.respond.send(Response::ok(id, json!({"ok": true})));
        return false;
    }
    let shutdown = method == "shutdown";
    let written = command.written.take();
    let response = match handle_request(state, command.request) {
        Ok(value) => Response::ok(id, value),
        Err(error) => Response::err(id, error.to_string()),
    };
    let _ = command.respond.send(response);
    if shutdown {
        if let Some(written) = written {
            let _ = written.recv_timeout(Duration::from_secs(2));
        }
    }
    shutdown
}

pub(crate) fn broadcast(state: &mut State, event: Value) {
    state
        .subscribers
        .retain(|subscriber| subscriber.send(event.clone()).is_ok());
}

pub(crate) fn accept_loop(mut listener: Listener, commands: Sender<Command>) {
    loop {
        let Ok(conn) = listener.accept() else { break };
        let commands = commands.clone();
        thread::spawn(move || client_loop(conn, commands));
    }
}

fn client_loop(stream: Conn, commands: Sender<Command>) {
    let reader_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(_) => return,
    };
    let mut reader = BufReader::new(reader_stream);
    let mut writer = stream;
    serve(&mut reader, &mut writer, commands);
    writer.shutdown();
}

pub(crate) fn serve(reader: &mut BufReader<Conn>, writer: &mut Conn, commands: Sender<Command>) {
    let mut line = String::new();

    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let request = match serde_json::from_str::<Request>(&line) {
            Ok(request) => request,
            Err(error) => {
                let response = Response::err(0, format!("invalid request: {error}"));
                if write_response(&mut *writer, &response).is_err() {
                    break;
                }
                line.clear();
                continue;
            }
        };
        let subscribe = request.method == "subscribe";
        let (respond_tx, respond_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let (written_tx, written_rx) = mpsc::channel();
        if commands
            .send(Command {
                request,
                respond: respond_tx,
                subscriber: subscribe.then_some(event_tx),
                written: Some(written_rx),
            })
            .is_err()
        {
            break;
        }
        let response = match respond_rx.recv_timeout(Duration::from_secs(300)) {
            Ok(response) => response,
            Err(_) => Response::err(0, "daemon request timed out"),
        };
        let accepted = response.ok;
        if write_response(&mut *writer, &response).is_err() {
            break;
        }
        let _ = written_tx.send(());
        if subscribe {
            if !accepted {
                break;
            }
            loop {
                match event_rx.recv_timeout(Duration::from_secs(30)) {
                    Ok(event) => {
                        if serde_json::to_writer(&mut *writer, &event).is_err()
                            || writer.write_all(b"\n").is_err()
                            || writer.flush().is_err()
                        {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            break;
        }
        line.clear();
    }
}

pub(crate) fn write_response(writer: &mut Conn, response: &Response) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, response).map_err(std::io::Error::other)?;
    writer.write_all(b"\n")?;
    writer.flush()
}
