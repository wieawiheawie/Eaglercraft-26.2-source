use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use eagler_network::build_wisp_uri;
use eagler_renderer::GpuRenderer;
use wasm_bindgen::{closure::Closure, prelude::*, JsCast};
use web_sys::{
    HtmlButtonElement, HtmlCanvasElement, HtmlElement, HtmlInputElement, KeyboardEvent, MouseEvent,
};

use crate::{
    game::{Controls, GameSimulation},
    BrowserWispConnection,
};

struct NetworkUi {
    connection: Rc<RefCell<Option<BrowserWispConnection>>>,
    probe_started: Rc<Cell<bool>>,
    status: HtmlElement,
    last_status: Rc<RefCell<String>>,
}

/// Starts the Rust-owned game loop on the named canvas.
///
/// Browser APIs stay at this boundary. World state, player physics, simulation
/// timing, block storage, meshing and mod execution are Rust implementations.
#[wasm_bindgen]
pub async fn start(canvas_id: &str) -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("Browser window is unavailable"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("Browser document is unavailable"))?;
    let element = document
        .get_element_by_id(canvas_id)
        .ok_or_else(|| JsValue::from_str("Game canvas was not found"))?;
    let canvas = element
        .dyn_into::<HtmlCanvasElement>()
        .map_err(|_| JsValue::from_str("Game canvas element is not a canvas"))?;

    let renderer = Rc::new(RefCell::new(
        GpuRenderer::new(canvas.clone())
            .await
            .map_err(|message| JsValue::from_str(&message))?,
    ));
    let simulation = Rc::new(RefCell::new(
        GameSimulation::new().map_err(|message| JsValue::from_str(&message))?,
    ));
    let controls = Rc::new(RefCell::new(Controls::default()));
    let network_ui = install_network_controls(&document)?;

    install_input_handlers(&window, &document, &canvas, &simulation, &controls)?;

    let callback = Rc::new(RefCell::new(None::<Closure<dyn FnMut(f64)>>));
    let callback_for_frame = callback.clone();
    let renderer_for_frame = renderer.clone();
    let simulation_for_frame = simulation.clone();
    let controls_for_frame = controls.clone();
    let window_for_frame = window.clone();

    *callback.borrow_mut() = Some(Closure::wrap(Box::new(move |timestamp_ms: f64| {
        let sampled_controls = *controls_for_frame.borrow();
        if let Ok(mut game) = simulation_for_frame.try_borrow_mut() {
            game.frame(timestamp_ms, sampled_controls);
        }
        if let Ok(mut renderer) = renderer_for_frame.try_borrow_mut() {
            let _ = renderer.render_frame();
        }
        update_network_ui(&network_ui);
        if let Some(next_frame) = callback_for_frame.borrow().as_ref() {
            let _ = window_for_frame.request_animation_frame(next_frame.as_ref().unchecked_ref());
        }
    }) as Box<dyn FnMut(f64)>));

    if let Some(first_frame) = callback.borrow().as_ref() {
        window.request_animation_frame(first_frame.as_ref().unchecked_ref())?;
    }

    // The animation and input callbacks intentionally live for this page.
    std::mem::forget(callback);
    Ok(())
}

fn install_network_controls(document: &web_sys::Document) -> Result<NetworkUi, JsValue> {
    let relay = document
        .get_element_by_id("wisp-relay")
        .ok_or_else(|| JsValue::from_str("Wisp relay input is missing"))?
        .dyn_into::<HtmlInputElement>()?;
    let host = document
        .get_element_by_id("server-host")
        .ok_or_else(|| JsValue::from_str("Server host input is missing"))?
        .dyn_into::<HtmlInputElement>()?;
    let port = document
        .get_element_by_id("server-port")
        .ok_or_else(|| JsValue::from_str("Server port input is missing"))?
        .dyn_into::<HtmlInputElement>()?;
    let button = document
        .get_element_by_id("connect-button")
        .ok_or_else(|| JsValue::from_str("Connect button is missing"))?
        .dyn_into::<HtmlButtonElement>()?;
    let status = document
        .get_element_by_id("connection-status")
        .ok_or_else(|| JsValue::from_str("Connection status label is missing"))?
        .dyn_into::<HtmlElement>()?;

    let connection = Rc::new(RefCell::new(None::<BrowserWispConnection>));
    let probe_started = Rc::new(Cell::new(false));
    let last_status = Rc::new(RefCell::new(String::new()));

    let connection_for_click = connection.clone();
    let probe_for_click = probe_started.clone();
    let status_for_click = status.clone();
    let last_for_click = last_status.clone();
    let relay_for_click = relay.clone();
    let host_for_click = host.clone();
    let port_for_click = port.clone();

    let click = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        let endpoint = relay_for_click.value().trim().to_owned();
        let hostname = host_for_click.value().trim().to_owned();
        let port_number = match port_for_click.value().trim().parse::<u16>() {
            Ok(value) if value > 0 => value,
            _ => {
                set_status(&status_for_click, &last_for_click, "Enter a valid TCP port (1-65535).");
                return;
            }
        };
        let uri = match build_wisp_uri(&endpoint, &hostname, port_number) {
            Ok(uri) => uri,
            Err(error) => {
                set_status(
                    &status_for_click,
                    &last_for_click,
                    &format!("Invalid relay or server address: {error:?}"),
                );
                return;
            }
        };

        match BrowserWispConnection::new(&uri) {
            Ok(client) => {
                *connection_for_click.borrow_mut() = Some(client);
                probe_for_click.set(false);
                set_status(
                    &status_for_click,
                    &last_for_click,
                    "Opening Wisp WebSocket relay…",
                );
            }
            Err(error) => set_status(
                &status_for_click,
                &last_for_click,
                &format!("Could not open Wisp connection: {error:?}"),
            ),
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    button.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
    std::mem::forget(click);

    Ok(NetworkUi {
        connection,
        probe_started,
        status,
        last_status,
    })
}

fn update_network_ui(ui: &NetworkUi) {
    let connection = ui.connection.borrow();
    let Some(connection) = connection.as_ref() else {
        return;
    };

    let state = connection.state();
    if state == "connected" {
        if !ui.probe_started.get() {
            match connection.begin_status_query() {
                Ok(()) => {
                    ui.probe_started.set(true);
                    set_status(
                        &ui.status,
                        &ui.last_status,
                        "Wisp TCP stream opened. Querying the Minecraft/BungeeCord endpoint…",
                    );
                }
                Err(error) => set_status(
                    &ui.status,
                    &ui.last_status,
                    &format!("Wisp opened but Minecraft handshake could not start: {error:?}"),
                ),
            }
            return;
        }

        match connection.poll_status() {
            Ok(Some(json)) => set_status(
                &ui.status,
                &ui.last_status,
                &format!("Minecraft server replied and ping verified:
{json}"),
            ),
            Ok(None) => {}
            Err(error) => set_status(
                &ui.status,
                &ui.last_status,
                &format!("Minecraft status handshake failed: {error:?}"),
            ),
        }
    } else {
        set_status(&ui.status, &ui.last_status, &state);
    }
}

fn set_status(element: &HtmlElement, previous: &RefCell<String>, value: &str) {
    let mut previous = previous.borrow_mut();
    if previous.as_str() != value {
        element.set_inner_text(value);
        *previous = value.to_owned();
    }
}

fn install_input_handlers(
    window: &web_sys::Window,
    document: &web_sys::Document,
    canvas: &HtmlCanvasElement,
    simulation: &Rc<RefCell<GameSimulation>>,
    controls: &Rc<RefCell<Controls>>,
) -> Result<(), JsValue> {
    let controls_for_down = controls.clone();
    let key_down = Closure::wrap(Box::new(move |event: KeyboardEvent| {
        if update_key(&mut controls_for_down.borrow_mut(), &event.code(), true) {
            event.prevent_default();
        }
    }) as Box<dyn FnMut(KeyboardEvent)>);
    window.add_event_listener_with_callback("keydown", key_down.as_ref().unchecked_ref())?;
    std::mem::forget(key_down);

    let controls_for_up = controls.clone();
    let key_up = Closure::wrap(Box::new(move |event: KeyboardEvent| {
        if update_key(&mut controls_for_up.borrow_mut(), &event.code(), false) {
            event.prevent_default();
        }
    }) as Box<dyn FnMut(KeyboardEvent)>);
    window.add_event_listener_with_callback("keyup", key_up.as_ref().unchecked_ref())?;
    std::mem::forget(key_up);

    let controls_for_blur = controls.clone();
    let blur = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        *controls_for_blur.borrow_mut() = Controls::default();
    }) as Box<dyn FnMut(web_sys::Event)>);
    window.add_event_listener_with_callback("blur", blur.as_ref().unchecked_ref())?;
    std::mem::forget(blur);

    let canvas_for_click = canvas.clone();
    let click = Closure::wrap(Box::new(move |_event: MouseEvent| {
        let _ = canvas_for_click.request_pointer_lock();
    }) as Box<dyn FnMut(MouseEvent)>);
    canvas.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
    std::mem::forget(click);

    let document_for_mouse = document.clone();
    let simulation_for_mouse = simulation.clone();
    let mouse_move = Closure::wrap(Box::new(move |event: MouseEvent| {
        if document_for_mouse.pointer_lock_element().is_some() {
            let yaw = -(event.movement_x() as f32) * 0.002;
            let pitch = -(event.movement_y() as f32) * 0.002;
            if let Ok(mut game) = simulation_for_mouse.try_borrow_mut() {
                game.look(yaw, pitch);
            }
        }
    }) as Box<dyn FnMut(MouseEvent)>);
    window.add_event_listener_with_callback("mousemove", mouse_move.as_ref().unchecked_ref())?;
    std::mem::forget(mouse_move);

    Ok(())
}

fn update_key(controls: &mut Controls, code: &str, pressed: bool) -> bool {
    match code {
        "KeyW" | "ArrowUp" => controls.forward = pressed,
        "KeyS" | "ArrowDown" => controls.backward = pressed,
        "KeyA" | "ArrowLeft" => controls.left = pressed,
        "KeyD" | "ArrowRight" => controls.right = pressed,
        "Space" => controls.jump = pressed,
        _ => return false,
    }
    true
}
