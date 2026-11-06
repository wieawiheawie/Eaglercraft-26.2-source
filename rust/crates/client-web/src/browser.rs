use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

use eagler_network::build_wisp_uri;
use eagler_renderer::{build_world_mesh, BlockTextureSet, GpuRenderer};
use eagler_assets::ResourceLocation;
use crate::assets_browser::load_starter_texture_atlas;
use wasm_bindgen::{closure::Closure, prelude::*, JsCast};
use web_sys::{
    HtmlButtonElement, HtmlCanvasElement, HtmlElement, HtmlInputElement, KeyboardEvent, MouseEvent,
};

use crate::{
    game::{Controls, GameSimulation},
    BrowserWispConnection,
};

struct NetworkUi {
    status_connection: Rc<RefCell<Option<BrowserWispConnection>>>,
    game_connection: Rc<RefCell<Option<BrowserWispConnection>>>,
    probe_started: Rc<Cell<bool>>,
    login_started: Rc<Cell<bool>>,
    target_uri: Rc<RefCell<Option<String>>>,
    username: Rc<RefCell<String>>,
    status: HtmlElement,
    last_status: Rc<RefCell<String>>,
    last_sent_tick: Cell<u64>,
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

    let mut gpu_renderer = GpuRenderer::new(canvas.clone())
        .await
        .map_err(|message| JsValue::from_str(&message))?;
    let texture_regions = load_starter_texture_atlas(&mut gpu_renderer).await?;
    let simulation = GameSimulation::new().map_err(|message| JsValue::from_str(&message))?;
    let stone_id = simulation
        .block_id("minecraft:stone")
        .ok_or_else(|| JsValue::from_str("Stone block registration is missing"))?;
    let stone_key = ResourceLocation::parse("minecraft:block/stone")
        .map_err(|error| JsValue::from_str(&format!("Invalid stone texture key: {error:?}")))?;
    let stone_region = texture_regions
        .get(&stone_key)
        .copied()
        .ok_or_else(|| JsValue::from_str("Stone texture was not packed into the atlas"))?;
    let block_textures = BTreeMap::from([(
        stone_id,
        BlockTextureSet::uniform(stone_region),
    )]);
    let world_mesh = build_world_mesh(simulation.world(), simulation.registry(), &block_textures);
    gpu_renderer
        .set_mesh(&world_mesh)
        .map_err(|error| JsValue::from_str(&error))?;

    let simulation = Rc::new(RefCell::new(simulation));
    let renderer = Rc::new(RefCell::new(gpu_renderer));
    let aspect_ratio = canvas.width().max(1) as f32 / canvas.height().max(1) as f32;
    let controls = Rc::new(RefCell::new(Controls::default()));
    let network_ui = install_network_controls(&document)?;
    let block_textures_for_frame = block_textures.clone();

    install_input_handlers(&window, &document, &canvas, &simulation, &controls)?;

    let callback = Rc::new(RefCell::new(None::<Closure<dyn FnMut(f64)>>));
    let callback_for_frame = callback.clone();
    let renderer_for_frame = renderer.clone();
    let simulation_for_frame = simulation.clone();
    let controls_for_frame = controls.clone();
    let window_for_frame = window.clone();
    let aspect_for_frame = aspect_ratio;

    *callback.borrow_mut() = Some(Closure::wrap(Box::new(move |timestamp_ms: f64| {
        let sampled_controls = *controls_for_frame.borrow();
        let camera = if let Ok(mut game) = simulation_for_frame.try_borrow_mut() {
            game.frame(timestamp_ms, sampled_controls);
            Some((game.player_eye_position(), game.player_forward()))
        } else {
            None
        };
        if let (Some((eye, forward)), Ok(mut renderer)) =
            (camera, renderer_for_frame.try_borrow_mut())
        {
            renderer.set_camera(eye, forward, aspect_for_frame);
            let _ = renderer.render_frame();
        }
        update_network_ui(&network_ui, &simulation_for_frame, &renderer_for_frame, &block_textures_for_frame);
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
    let username_input = document
        .get_element_by_id("player-username")
        .ok_or_else(|| JsValue::from_str("Player username input is missing"))?
        .dyn_into::<HtmlInputElement>()?;
    let button = document
        .get_element_by_id("connect-button")
        .ok_or_else(|| JsValue::from_str("Connect button is missing"))?
        .dyn_into::<HtmlButtonElement>()?;
    let status = document
        .get_element_by_id("connection-status")
        .ok_or_else(|| JsValue::from_str("Connection status label is missing"))?
        .dyn_into::<HtmlElement>()?;

    let status_connection = Rc::new(RefCell::new(None::<BrowserWispConnection>));
    let game_connection = Rc::new(RefCell::new(None::<BrowserWispConnection>));
    let probe_started = Rc::new(Cell::new(false));
    let login_started = Rc::new(Cell::new(false));
    let target_uri = Rc::new(RefCell::new(None::<String>));
    let username = Rc::new(RefCell::new("RustPlayer".to_owned()));
    let last_status = Rc::new(RefCell::new(String::new()));

    let status_for_click = status.clone();
    let last_for_click = last_status.clone();
    let status_connection_for_click = status_connection.clone();
    let game_connection_for_click = game_connection.clone();
    let probe_for_click = probe_started.clone();
    let login_for_click = login_started.clone();
    let uri_for_click = target_uri.clone();
    let username_for_click = username.clone();
    let relay_for_click = relay.clone();
    let host_for_click = host.clone();
    let port_for_click = port.clone();
    let username_input_for_click = username_input.clone();

    let click = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        let endpoint = relay_for_click.value().trim().to_owned();
        let hostname = host_for_click.value().trim().to_owned();
        let player_name = username_input_for_click.value().trim().to_owned();
        if player_name.is_empty()
            || player_name.len() > 16
            || !player_name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            set_status(
                &status_for_click,
                &last_for_click,
                "Enter a valid Minecraft username (1-16 letters, numbers or underscores).",
            );
            return;
        }
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
                *status_connection_for_click.borrow_mut() = Some(client);
                *game_connection_for_click.borrow_mut() = None;
                *uri_for_click.borrow_mut() = Some(uri);
                *username_for_click.borrow_mut() = player_name.clone();
                probe_for_click.set(false);
                login_for_click.set(false);
                set_status(
                    &status_for_click,
                    &last_for_click,
                    &format!("Opening Wisp relay to {hostname}:{port_number} as {player_name}…"),
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
        status_connection,
        game_connection,
        probe_started,
        login_started,
        target_uri,
        username,
        status,
        last_status,
        last_sent_tick: Cell::new(0),
    })
}

fn update_network_ui(
    ui: &NetworkUi,
    simulation: &Rc<RefCell<GameSimulation>>,
    renderer: &Rc<RefCell<GpuRenderer>>,
    block_textures: &BTreeMap<eagler_game_core::BlockId, BlockTextureSet>,
) {
    // Status and login are deliberately separate TCP streams: a status
    // handshake sets next-state=1, while Login Start requires next-state=2.
    let mut completed_status = None::<String>;
    {
        let status_connection = ui.status_connection.borrow();
        if let Some(connection) = status_connection.as_ref() {
            let state = connection.state();
            if state == "connected" {
                if !ui.probe_started.get() {
                    match connection.begin_status_query() {
                        Ok(()) => {
                            ui.probe_started.set(true);
                            set_status(
                                &ui.status,
                                &ui.last_status,
                                "Wisp TCP stream opened. Sending Minecraft status handshake and ping…",
                            );
                        }
                        Err(error) => set_status(
                            &ui.status,
                            &ui.last_status,
                            &format!("Wisp opened but status handshake could not start: {error:?}"),
                        ),
                    }
                } else {
                    match connection.poll_status() {
                        Ok(Some(json)) => completed_status = Some(json),
                        Ok(None) => {}
                        Err(error) => set_status(
                            &ui.status,
                            &ui.last_status,
                            &format!("Minecraft status handshake failed: {error:?}"),
                        ),
                    }
                }
            } else if state.starts_with("failed:") || state == "closed" {
                set_status(&ui.status, &ui.last_status, &state);
            }
        }
    }

    if let Some(json) = completed_status {
        set_status(
            &ui.status,
            &ui.last_status,
            &format!("Status ping verified:\n{json}\nOpening a fresh Wisp TCP stream for Login Start…"),
        );
        ui.status_connection.borrow_mut().take();
        if let Some(uri) = ui.target_uri.borrow().as_deref() {
            match BrowserWispConnection::new(uri) {
                Ok(connection) => {
                    *ui.game_connection.borrow_mut() = Some(connection);
                    ui.login_started.set(false);
                    ui.last_sent_tick.set(0);
                }
                Err(error) => set_status(
                    &ui.status,
                    &ui.last_status,
                    &format!("Status succeeded, but a new login stream could not open: {error:?}"),
                ),
            }
        }
    }

    let mut world_mesh_dirty = false;
    {
        let game_connection = ui.game_connection.borrow();
        let Some(connection) = game_connection.as_ref() else {
            return;
        };
        let state = connection.state();
        if state == "connected" && !ui.login_started.get() {
            let player_name = ui.username.borrow().clone();
            match connection.begin_login(&player_name) {
                Ok(()) => {
                    ui.login_started.set(true);
                    set_status(
                        &ui.status,
                        &ui.last_status,
                        &format!("Wisp connected. Login Start sent for {player_name}; waiting for Login Success…"),
                    );
                }
                Err(error) => set_status(
                    &ui.status,
                    &ui.last_status,
                    &format!("Could not begin Minecraft login: {error:?}"),
                ),
            }
        }

        if ui.login_started.get() {
            match connection.poll_login() {
                Ok(login_state) => {
                    if login_state == "authentication-required" {
                        let hash = connection.authentication_server_hash().unwrap_or_default();
                        set_status(
                            &ui.status,
                            &ui.last_status,
                            &format!(
                                "Server requires official online-mode authentication.\nSHA-1 server hash: {hash}\nThe client is waiting for a real authenticated session; it will not fake one."
                            ),
                        );
                    } else if login_state == "ready" {
                        set_status(
                            &ui.status,
                            &ui.last_status,
                            "Minecraft Play state is live. Receiving and applying server chunk packets; WASD + mouse look are enabled.",
                        );

                        loop {
                            let Some(packet) = connection.pop_world_packet() else {
                                break;
                            };
                            let Ok(mut game) = simulation.try_borrow_mut() else {
                                break;
                            };
                            match game.ingest_world_packet(packet) {
                                Ok(changed) => world_mesh_dirty |= changed,
                                Err(error) => {
                                    set_status(
                                        &ui.status,
                                        &ui.last_status,
                                        &format!("World packet ingestion failed: {error}"),
                                    );
                                    break;
                                }
                            }
                        }

                        if let Ok(game) = simulation.try_borrow() {
                            let (tick, position, yaw, pitch, on_ground) = game.player_network_snapshot();
                            let ticks_due = tick.saturating_sub(ui.last_sent_tick.get()).min(4);
                            for _ in 0..ticks_due {
                                match connection.send_player_movement(
                                    position[0],
                                    position[1],
                                    position[2],
                                    yaw,
                                    pitch,
                                    on_ground,
                                ) {
                                    Ok(()) => ui.last_sent_tick.set(ui.last_sent_tick.get() + 1),
                                    Err(error) => {
                                        set_status(
                                            &ui.status,
                                            &ui.last_status,
                                            &format!("Could not send player tick: {error:?}"),
                                        );
                                        break;
                                    }
                                }
                            }
                        }
                    } else if login_state == "awaiting-play-login" {
                        set_status(
                            &ui.status,
                            &ui.last_status,
                            "Login Success accepted. Configuration is complete; waiting for the Play Login packet…",
                        );
                    } else {
                        set_status(
                            &ui.status,
                            &ui.last_status,
                            &format!("Minecraft login state: {login_state}"),
                        );
                    }
                }
                Err(error) => set_status(
                    &ui.status,
                    &ui.last_status,
                    &format!("Minecraft login/Play protocol failed: {error:?}"),
                ),
            }
        } else if state.starts_with("failed:") || state == "closed" {
            set_status(&ui.status, &ui.last_status, &state);
        }
    }

    if world_mesh_dirty {
        if let Ok(game) = simulation.try_borrow() {
            let world_mesh = build_world_mesh(game.world(), game.registry(), block_textures);
            if let Ok(mut gpu) = renderer.try_borrow_mut() {
                if let Err(error) = gpu.set_mesh(&world_mesh) {
                    set_status(
                        &ui.status,
                        &ui.last_status,
                        &format!("World mesh upload failed: {error}"),
                    );
                }
            }
        }
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
    canvas.set_tab_index(0);

    let controls_for_down = controls.clone();
    let document_for_down = document.clone();
    let key_down = Closure::wrap(Box::new(move |event: KeyboardEvent| {
        if document_for_down.pointer_lock_element().is_some()
            && update_key(&mut controls_for_down.borrow_mut(), &event.code(), true)
        {
            event.prevent_default();
        }
    }) as Box<dyn FnMut(KeyboardEvent)>);
    window.add_event_listener_with_callback("keydown", key_down.as_ref().unchecked_ref())?;
    std::mem::forget(key_down);

    let controls_for_up = controls.clone();
    let document_for_up = document.clone();
    let key_up = Closure::wrap(Box::new(move |event: KeyboardEvent| {
        if document_for_up.pointer_lock_element().is_some()
            && update_key(&mut controls_for_up.borrow_mut(), &event.code(), false)
        {
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
        let _ = canvas_for_click.focus();
        let _ = canvas_for_click.request_pointer_lock();
    }) as Box<dyn FnMut(MouseEvent)>);
    canvas.add_event_listener_with_callback("click", click.as_ref().unchecked_ref())?;
    std::mem::forget(click);

    let controls_for_unlock = controls.clone();
    let document_for_lock_change = document.clone();
    let lock_change = Closure::wrap(Box::new(move |_event: web_sys::Event| {
        if document_for_lock_change.pointer_lock_element().is_none() {
            *controls_for_unlock.borrow_mut() = Controls::default();
        }
    }) as Box<dyn FnMut(web_sys::Event)>);
    document.add_event_listener_with_callback("pointerlockchange", lock_change.as_ref().unchecked_ref())?;
    std::mem::forget(lock_change);

    let document_for_mouse = document.clone();
    let simulation_for_mouse = simulation.clone();
    let mouse_move = Closure::wrap(Box::new(move |event: MouseEvent| {
        if document_for_mouse.pointer_lock_element().is_some() {
            let yaw = (event.movement_x() as f32) * 0.002;
            let pitch = (event.movement_y() as f32) * 0.002;
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
