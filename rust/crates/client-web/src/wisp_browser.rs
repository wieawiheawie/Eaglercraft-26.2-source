use std::{cell::RefCell, rc::{Rc, Weak}};

use js_sys::{ArrayBuffer, Uint8Array};
use eagler_network::{
    parse_wisp_uri, LoginState, MinecraftLoginSession, MinecraftStatusProbe, StatusProbeState,
    WispSession, WispState, WispTarget, WorldPacket,
};
use wasm_bindgen::{closure::Closure, prelude::*, JsCast};
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

struct ConnectionInner {
    socket: WebSocket,
    target: WispTarget,
    session: WispSession,
    opened: bool,
    websocket_closed: bool,
    failure: Option<String>,
    status_probe: Option<MinecraftStatusProbe>,
    probe_output: Vec<u8>,
    login_session: Option<MinecraftLoginSession>,
    login_output: Vec<u8>,
}

#[wasm_bindgen]
pub struct BrowserWispConnection {
    inner: Rc<RefCell<ConnectionInner>>,
    on_open: Closure<dyn FnMut(Event)>,
    on_message: Closure<dyn FnMut(MessageEvent)>,
    on_error: Closure<dyn FnMut(Event)>,
    on_close: Closure<dyn FnMut(CloseEvent)>,
}

#[wasm_bindgen]
impl BrowserWispConnection {
    /// Opens an independent WebSocket to the Wisp relay described by an
    /// eagler-wisp URI. The TCP stream becomes ready after the Wisp v1 greeting.
    #[wasm_bindgen(constructor)]
    pub fn new(uri: &str) -> Result<BrowserWispConnection, JsValue> {
        let target = parse_wisp_uri(uri)
            .map_err(|error| JsValue::from_str(&format!("Invalid Wisp address: {error:?}")))?;
        let socket = WebSocket::new(&target.endpoint)?;
        socket.set_binary_type(BinaryType::Arraybuffer);

        let inner = Rc::new(RefCell::new(ConnectionInner {
            socket: socket.clone(),
            session: WispSession::new(target.host.clone(), target.port)
                .map_err(|error| JsValue::from_str(&format!("Invalid Wisp target: {error:?}")))?,
            target,
            opened: false,
            websocket_closed: false,
            failure: None,
            status_probe: None,
            probe_output: Vec::with_capacity(64 * 1024),
            login_session: None,
            login_output: Vec::with_capacity(64 * 1024),
        }));

        let weak: Weak<RefCell<ConnectionInner>> = Rc::downgrade(&inner);
        let on_open = Closure::wrap(Box::new(move |_event: Event| {
            if let Some(inner) = weak.upgrade() {
                inner.borrow_mut().opened = true;
            }
        }) as Box<dyn FnMut(Event)>);
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let weak = Rc::downgrade(&inner);
        let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let data = event.data();
            let Ok(buffer) = data.dyn_into::<ArrayBuffer>() else {
                let mut inner = inner.borrow_mut();
                inner.failure = Some("Wisp relay sent a non-binary WebSocket message".to_owned());
                inner.session.close();
                let _ = inner.socket.close();
                return;
            };
            // One linear copy from the browser's received ArrayBuffer into the
            // reusable Rust stream buffer; no per-byte JavaScript Array objects.
            let bytes = Uint8Array::new(&buffer).to_vec();
            let mut inner = inner.borrow_mut();
            if let Err(error) = inner.session.receive_packet(&bytes) {
                inner.failure = Some(format!("Wisp protocol error: {error:?}"));
                let _ = inner.socket.close();
                return;
            }
            flush_outbound(&mut inner);
        }) as Box<dyn FnMut(MessageEvent)>);
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let weak = Rc::downgrade(&inner);
        let on_error = Closure::wrap(Box::new(move |_event: Event| {
            if let Some(inner) = weak.upgrade() {
                inner.borrow_mut().failure =
                    Some("Wisp WebSocket relay reported a transport error".to_owned());
            }
        }) as Box<dyn FnMut(Event)>);
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let weak = Rc::downgrade(&inner);
        let on_close = Closure::wrap(Box::new(move |event: CloseEvent| {
            if let Some(inner) = weak.upgrade() {
                let mut inner = inner.borrow_mut();
                inner.websocket_closed = true;
                if inner.failure.is_none() && inner.session.state() != WispState::Closed {
                    let reason = event.reason();
                    inner.failure = Some(if reason.is_empty() {
                        format!("Wisp WebSocket closed (code {})", event.code())
                    } else {
                        format!("Wisp WebSocket closed: {reason}")
                    });
                }
            }
        }) as Box<dyn FnMut(CloseEvent)>);
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        Ok(Self {
            inner,
            on_open,
            on_message,
            on_error,
            on_close,
        })
    }

    /// Returns connecting, waiting-greeting, connected, closed, or a failure.
    pub fn state(&self) -> String {
        let inner = self.inner.borrow();
        if let Some(error) = &inner.failure {
            return format!("failed: {error}");
        }
        if inner.websocket_closed || inner.session.state() == WispState::Closed {
            return "closed".to_owned();
        }
        match inner.session.state() {
            WispState::AwaitingGreeting if inner.opened => "waiting-greeting".to_owned(),
            WispState::AwaitingGreeting => "connecting".to_owned(),
            WispState::Ready => "connected".to_owned(),
            WispState::Closed => "closed".to_owned(),
            WispState::Failed => "failed".to_owned(),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.inner.borrow().session.state() == WispState::Ready
    }

    pub fn buffered_amount(&self) -> u32 {
        self.inner.borrow().socket.buffered_amount()
    }

    /// Sends raw Minecraft TCP-stream bytes through Wisp's credit-controlled
    /// stream. Waiting data is bounded to four MiB.
    pub fn send_tcp(&self, bytes: &[u8]) -> Result<(), JsValue> {
        let mut inner = self.inner.borrow_mut();
        if let Some(error) = &inner.failure {
            return Err(JsValue::from_str(error));
        }
        inner
            .session
            .send_tcp(bytes)
            .map_err(|error| JsValue::from_str(&format!("Wisp send failed: {error:?}")))?;
        flush_outbound(&mut inner);
        Ok(())
    }

    /// Reads raw TCP bytes into a caller-owned reusable buffer.
    pub fn read_tcp(&self, destination: &mut [u8]) -> usize {
        self.inner.borrow_mut().session.read_tcp(destination)
    }

    /// Begins a normal Minecraft 26.2 status handshake over this TCP stream.
    /// The server must be a Java-protocol endpoint reachable through the relay.
    pub fn begin_status_query(&self) -> Result<(), JsValue> {
        let mut inner = self.inner.borrow_mut();
        if inner.session.state() != WispState::Ready {
            return Err(JsValue::from_str("Wisp TCP stream is not ready yet"));
        }
        if inner.status_probe.is_some() {
            return Err(JsValue::from_str("A Minecraft status query is already active"));
        }

        let nonce = js_sys::Date::now() as i64;
        let mut probe = MinecraftStatusProbe::new(&inner.target.host, inner.target.port, nonce)
            .map_err(|error| JsValue::from_str(&format!("Could not create Minecraft handshake: {error:?}")))?;
        let ConnectionInner {
            session,
            probe_output,
            status_probe,
            ..
        } = &mut *inner;
        probe.take_output(probe_output);
        session
            .send_tcp(probe_output)
            .map_err(|error| JsValue::from_str(&format!("Could not send Minecraft handshake: {error:?}")))?;
        *status_probe = Some(probe);
        flush_outbound(&mut inner);
        Ok(())
    }

    /// Polls the TCP stream, advances the status handshake, and sends the ping.
    /// Returns the server's JSON status only after the ping nonce is verified.
    pub fn poll_status(&self) -> Result<Option<String>, JsValue> {
        let mut inner = self.inner.borrow_mut();
        if let Some(error) = &inner.failure {
            return Err(JsValue::from_str(error));
        }
        let mut scratch = [0_u8; 8192];
        loop {
            let count = inner.session.read_tcp(&mut scratch);
            if count == 0 {
                break;
            }
            let probe = inner
                .status_probe
                .as_mut()
                .ok_or_else(|| JsValue::from_str("Start the Minecraft status query first"))?;
            probe
                .receive_tcp(&scratch[..count])
                .map_err(|error| JsValue::from_str(&format!("Minecraft status protocol error: {error:?}")))?;
        }

        let ConnectionInner {
            session,
            probe_output,
            status_probe,
            ..
        } = &mut *inner;
        probe_output.clear();
        let Some(probe) = status_probe.as_mut() else {
            return Err(JsValue::from_str("Start the Minecraft status query first"));
        };
        probe.take_output(probe_output);
        if !probe_output.is_empty() {
            session
                .send_tcp(probe_output)
                .map_err(|error| JsValue::from_str(&format!("Could not send Minecraft status ping: {error:?}")))?;
        }
        let complete = probe.state() == StatusProbeState::Complete;
        let status = if complete {
            probe.status_json().map(str::to_owned)
        } else {
            None
        };
        flush_outbound(&mut inner);
        Ok(status)
    }

    /// Starts a fresh Minecraft login handshake on this Wisp TCP stream.
    /// Call this only on a new connection after the status probe has completed.
    pub fn begin_login(&self, username: &str) -> Result<(), JsValue> {
        let mut inner = self.inner.borrow_mut();
        if inner.session.state() != WispState::Ready {
            return Err(JsValue::from_str("Wisp TCP stream is not ready yet"));
        }
        if inner.login_session.is_some() {
            return Err(JsValue::from_str("A Minecraft login is already active"));
        }
        let mut login = MinecraftLoginSession::new(&inner.target.host, inner.target.port, username)
            .map_err(|error| JsValue::from_str(&format!("Could not start Minecraft login: {error:?}")))?;
        let ConnectionInner {
            session,
            login_output,
            login_session,
            ..
        } = &mut *inner;
        login.take_outbound(login_output);
        session
            .send_tcp(login_output)
            .map_err(|error| JsValue::from_str(&format!("Could not send Login Start: {error:?}")))?;
        *login_session = Some(login);
        flush_outbound(&mut inner);
        Ok(())
    }

    /// Polls the TCP byte stream, advances Login/Configuration/Play state,
    /// and sends queued protocol replies through Wisp's credit-controlled path.
    pub fn poll_login(&self) -> Result<String, JsValue> {
        let mut inner = self.inner.borrow_mut();
        if let Some(error) = &inner.failure {
            return Err(JsValue::from_str(error));
        }
        if inner.login_session.is_none() {
            return Err(JsValue::from_str("Start Minecraft login first"));
        }

        let mut scratch = [0_u8; 8192];
        loop {
            let count = inner.session.read_tcp(&mut scratch);
            if count == 0 {
                break;
            }
            let result = inner
                .login_session
                .as_mut()
                .expect("login session was checked")
                .receive_tcp(&scratch[..count]);
            if let Err(error) = result {
                if matches!(error, eagler_network::LoginError::AuthenticationRequired { .. }) {
                    break;
                }
                return Err(JsValue::from_str(&format!("Minecraft login failed: {error:?}")));
            }
        }

        {
            let ConnectionInner {
                session,
                login_output,
                login_session,
                ..
            } = &mut *inner;
            let login = login_session.as_mut().expect("login session was checked");
            login.take_outbound(login_output);
            if !login_output.is_empty() {
                session
                    .send_tcp(login_output)
                    .map_err(|error| JsValue::from_str(&format!("Could not send Minecraft protocol reply: {error:?}")))?;
            }
        }
        let state = inner.login_session.as_ref().expect("login session was checked").state();
        flush_outbound(&mut inner);
        Ok(login_state_label(state).to_owned())
    }

    pub fn login_state(&self) -> Option<String> {
        self.inner.borrow().login_session.as_ref().map(|session| login_state_label(session.state()).to_owned())
    }

    /// Sends authoritative movement once per fixed 20 TPS simulation tick.
    pub fn send_player_movement(
        &self,
        x: f32,
        y: f32,
        z: f32,
        yaw_degrees: f32,
        pitch_degrees: f32,
        on_ground: bool,
    ) -> Result<(), JsValue> {
        let mut inner = self.inner.borrow_mut();
        let ConnectionInner {
            session,
            login_output,
            login_session,
            ..
        } = &mut *inner;
        let login = login_session
            .as_mut()
            .ok_or_else(|| JsValue::from_str("Minecraft login has not started"))?;
        login
            .queue_player_movement([x, y, z], yaw_degrees, pitch_degrees, on_ground)
            .map_err(|error| JsValue::from_str(&format!("Could not queue player movement: {error:?}")))?;
        login.take_outbound(login_output);
        if !login_output.is_empty() {
            session
                .send_tcp(login_output)
                .map_err(|error| JsValue::from_str(&format!("Could not send player movement: {error:?}")))?;
        }
        flush_outbound(&mut inner);
        Ok(())
    }

    pub fn authentication_server_hash(&self) -> Option<String> {
        self.inner
            .borrow()
            .login_session
            .as_ref()
            .and_then(|session| session.server_hash().map(str::to_owned))
    }

    pub fn close(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.session.close();
        flush_outbound(&mut inner);
        let _ = inner.socket.close();
        inner.websocket_closed = true;
    }
}

impl Drop for BrowserWispConnection {
    fn drop(&mut self) {
        let mut inner = self.inner.borrow_mut();
        inner.session.close();
        flush_outbound(&mut inner);
        inner.socket.set_onopen(None);
        inner.socket.set_onmessage(None);
        inner.socket.set_onerror(None);
        inner.socket.set_onclose(None);
        let _ = inner.socket.close();
        inner.websocket_closed = true;
        let _ = (
            &self.on_open,
            &self.on_message,
            &self.on_error,
            &self.on_close,
        );
    }
}

impl BrowserWispConnection {
    /// Pops the next decoded world update for the Rust simulation.
    pub fn pop_world_packet(&self) -> Option<WorldPacket> {
        self.inner
            .borrow_mut()
            .login_session
            .as_mut()
            .and_then(MinecraftLoginSession::pop_world_packet)
    }

    pub fn queued_world_packets(&self) -> usize {
        self.inner
            .borrow()
            .login_session
            .as_ref()
            .map_or(0, MinecraftLoginSession::queued_world_packets)
    }
}

fn login_state_label(state: LoginState) -> &'static str {
    match state {
        LoginState::AwaitingLogin => "awaiting-login",
        LoginState::Authenticating => "authenticating",
        LoginState::AuthenticationRequired => "authentication-required",
        LoginState::AwaitingLoginSuccess => "awaiting-login-success",
        LoginState::Configuring => "configuring",
        LoginState::AwaitingPlayLogin => "awaiting-play-login",
        LoginState::Ready => "ready",
        LoginState::Failed => "failed",
    }
}

fn flush_outbound(inner: &mut ConnectionInner) {
    let mut failure = None;
    {
        let socket = &inner.socket;
        let session = &mut inner.session;
        while let Some(packet) = session.pop_outbound() {
            let result = socket.send_with_u8_array(&packet);
            session.recycle_outbound(packet);
            if let Err(error) = result {
                failure = Some(format!("Could not write to Wisp WebSocket: {error:?}"));
                break;
            }
        }
    }
    if let Some(error) = failure {
        inner.failure = Some(error);
        inner.session.close();
        let _ = inner.socket.close();
    }
}
