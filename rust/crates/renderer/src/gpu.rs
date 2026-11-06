use web_sys::HtmlCanvasElement;

/// Owns the canvas surface and GPU queue for the Rust browser client.
///
/// WebGPU is attempted first. If the browser cannot provide a WebGPU adapter,
/// a separate WebGL2 backend attempt is made.
pub struct GpuRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    backend_name: String,
}

impl GpuRenderer {
    pub async fn new(canvas: HtmlCanvasElement) -> Result<Self, String> {
        match Self::new_for_backend(canvas.clone(), wgpu::Backends::BROWSER_WEBGPU).await {
            Ok(renderer) => Ok(renderer),
            Err(webgpu_error) => {
                Self::new_for_backend(canvas, wgpu::Backends::GL)
                    .await
                    .map_err(|webgl_error| {
                        format!(
                            "GPU initialization failed. WebGPU: {webgpu_error}. WebGL2: {webgl_error}"
                        )
                    })
            }
        }
    }

    async fn new_for_backend(
        canvas: HtmlCanvasElement,
        backends: wgpu::Backends,
    ) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..Default::default()
        });
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|error| format!("Could not create browser GPU surface: {error}"))?;

        // A compatible surface is required for WebGL2 adapter selection.
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .map_err(|error| format!("No compatible GPU adapter: {error}"))?;

        let adapter_info = adapter.get_info();
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("Eaglercraft Rust client"),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        };
        let (device, queue) = adapter
            .request_device(&descriptor)
            .await
            .map_err(|error| format!("Could not create GPU device: {error}"))?;

        let width = canvas.width().max(1);
        let height = canvas.height().max(1);
        let config = surface
            .get_default_config(&adapter, width, height)
            .ok_or_else(|| "The selected GPU cannot present to this canvas".to_owned())?;
        surface.configure(&device, &config);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            backend_name: format!("{:?}", adapter_info.backend),
        })
    }

    pub fn backend_name(&self) -> &str {
        &self.backend_name
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    /// Acquires, clears and presents one frame. World mesh draw submission is
    /// layered on top of this surface lifecycle in the next renderer slice.
    pub fn render_frame(&mut self) -> Result<(), String> {
        let frame = self
            .surface
            .get_current_texture()
            .map_err(|error| format!("Could not acquire the next canvas frame: {error}"))?;
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Eaglercraft frame encoder"),
            });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Eaglercraft background clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.025,
                            g: 0.035,
                            b: 0.055,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        self.queue.submit([encoder.finish()]);
        frame.present();
        Ok(())
    }
}
