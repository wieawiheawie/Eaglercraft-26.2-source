use std::{cell::RefCell, rc::Rc};

use eagler_renderer::GpuRenderer;
use wasm_bindgen::{closure::Closure, prelude::*, JsCast};

/// Starts the Rust client renderer on the canvas with the given DOM id.
///
/// This module contains only the browser boundary. Game simulation, renderer
/// logic, world state and mod execution live in Rust crates.
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
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .map_err(|_| JsValue::from_str("Game canvas element is not a canvas"))?;

    let renderer = Rc::new(RefCell::new(
        GpuRenderer::new(canvas)
            .await
            .map_err(|message| JsValue::from_str(&message))?,
    ));

    let callback = Rc::new(RefCell::new(None::<Closure<dyn FnMut(f64)>>));
    let callback_for_frame = callback.clone();
    let renderer_for_frame = renderer.clone();
    let window_for_frame = window.clone();

    *callback.borrow_mut() = Some(Closure::wrap(Box::new(move |_timestamp: f64| {
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

    // The callback owns the repeating animation loop and remains live for the
    // lifetime of this client page.
    std::mem::forget(callback);
    Ok(())
}
