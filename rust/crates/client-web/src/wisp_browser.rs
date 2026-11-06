use std::{cell::RefCell, rc::{Rc, Weak}};

use js_sys::{ArrayBuffer, Uint8Array};
use eagler_network::{parse_wisp_uri, WispSession, WispState};
use wasm_bindgen::{closure::Closure, prelude::*, JsCast};
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

struct ConnectionInner {
    socket: WebSocket,
    session: WispSession,
    opened: bool,
    websocket_closed: bool,
    failure: Option<String>,
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
    /// eagler-wisp URI. TCP stream establishment completes asynchronously
    /// after the relay sends its Wisp v1 greeting.
    #[wasm_bindgen(constructor)]
    pub fn new(uri: &str) -> Result<BrowserWispConnection, JsValue> {
        let target = parse_wisp_uri(uri)
            .map_err(|error| JsValue::from_str(&format!("Invalid Wisp address: {error:?}")))?;
        let socket = WebSocket::new(&target.endpoint)?;
        socket.set_binary_type(BinaryType::Arraybuffer);

        let inner = Rc::new(RefCell::new(ConnectionInner {
            socket: socket.clone(),
            session: WispSession::new(target.host, target.port)
                .map_err(|error| JsValue::from_str(&format!("Invalid Wisp target: {error:?}")))?,
            opened: false,
            websocket_closed: false,
            failure: None,
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
                inner.borrow_mut().failure = Some("Wisp WebSocket relay reported a transport error".to_owned());
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

    /// Sends raw Minecraft TCP-stream bytes. The Wisp codec applies its credit
    /// window and retains at most four MiB of queued data.
    pub fn send_tcp(&self, bytes: &[u8]) -> Result<(), JsValue> {
        let mut inner = self.inner.borrow_mut();
        if let Some(error) = &inner.failure {
            return Err(JsValue::from_str(error));
        }
        inner.session.send_tcp(bytes)
            .map_err(|error| JsValue::from_str(&format!("Wisp send failed: {error:?}")))?;
        flush_outbound(&mut inner);
        Ok(())
    }

    /// Reads raw TCP bytes into a caller-owned reusable buffer.
    pub fn read_tcp(&self, destination: &mut [u8]) -> usize {
        self.inner.borrow_mut().session.read_tcp(destination)
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
        // Explicitly touch the callback fields so their lifetime is clear:
        // they are kept alive for exactly as long as this connection exists.
        let _ = (
            &self.on_open,
            &self.on_message,
            &self.on_error,
            &self.on_close,
        );
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
