use std::{cell::RefCell, rc::Rc};

use eagler_renderer::GpuRenderer;
use wasm_bindgen::{closure::Closure, prelude::*, JsCast};
use web_sys::{HtmlCanvasElement, KeyboardEvent, MouseEvent};

use crate::game::{Controls, GameSimulation};

/// Starts the Rust-owned game loop on the named canvas.
///
/// Browser APIs are confined to this thin boundary. World, player physics,
/// chunk data, simulation timing, meshing and mod execution remain Rust code.
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
        if let Some(next_frame) = callback_for_frame.borrow().as_ref() {
            let _ = window_for_frame.request_animation_frame(next_frame.as_ref().unchecked_ref());
        }
    }) as Box<dyn FnMut(f64)>));

    if let Some(first_frame) = callback.borrow().as_ref() {
        window.request_animation_frame(first_frame.as_ref().unchecked_ref())?;
    }

    // Event callbacks and the animation loop intentionally live for this page.
    std::mem::forget(callback);
    Ok(())
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
        // Pointer lock is requested only in response to a real user click.
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
