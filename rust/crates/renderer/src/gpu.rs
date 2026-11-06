use eagler_assets::TextureAtlas;
use js_sys::Uint8Array;
use web_sys::HtmlCanvasElement;
use wasm_bindgen::{JsCast, UnwrapThrowExt};

/// Owns the canvas surface and GPU queue for the Rust browser client.
///
/// WebGPU is attempted first. If a browser exposes `navigator.gpu` but cannot
/// create a compatible adapter, a separate WebGL2 backend attempt is made.
pub struct GpuRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    backend_name: String,
    texture_atlases: Vec<GpuTextureAtlas>,
}

struct GpuTextureAtlas {
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    width: u32,
    height: u32,
}

impl GpuRenderer {
    pub async fn new(canvas: HtmlCanvasElement) -> Result<Self, String> {
        match Self::new_for_backend(canvas.clone(), wgpu::Backends::BROWSER_WEBGPU).await {
            Ok(renderer) => Ok(renderer),
            Err(webgpu_error) => Self::new_for_backend(canvas, wgpu::Backends::GL)
                .await
                .map_err(|webgl_error| {
                    format!(
                        "GPU initialization failed. WebGPU: {webgpu_error}. WebGL2: {webgl_error}"
                    )
                }),
        }
    }

    async fn new_for_backend(
        canvas: HtmlCanvasElement,
        backends: wgpu::Backends,
    ) -> Result<Self, String> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = backends;
        let instance = wgpu::Instance::new(descriptor);
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|error| format!("Could not create browser GPU surface: {error}"))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
                // This client is a trusted host and keeps this setting fixed.
                apply_limit_buckets: true,
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

        let config = surface
            .get_default_config(&adapter, canvas.width().max(1), canvas.height().max(1))
            .ok_or_else(|| "The selected GPU cannot present to this canvas".to_owned())?;
        surface.configure(&device, &config);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            backend_name: format!("{:?}", adapter_info.backend),
            texture_atlases: Vec::new(),
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

    /// Allocates a GPU-resident dynamic vertex/index buffer.
    pub fn create_dynamic_buffer(
        &self,
        label: &'static str,
        capacity_bytes: u64,
        usage: wgpu::BufferUsages,
    ) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: capacity_bytes.max(4),
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Uploads a borrowed Rust/Wasm byte slice without materializing a
    /// JavaScript number array or an intermediate Vec<u8>.
    ///
    /// On wasm32, the source pointer is verified against the current
    /// WebAssembly.Memory buffer and exposed as a Uint8Array view for the
    /// duration of the queue write. The GPU API performs the unavoidable
    /// device transfer; the CPU-side bridge does not serialize every byte.
    pub fn upload_buffer_bytes(
        &self,
        target: &wgpu::Buffer,
        offset: u64,
        bytes: &[u8],
    ) {
        #[cfg(target_arch = "wasm32")]
        with_wasm_memory_view(bytes, |view| {
            debug_assert_eq!(view.byte_length() as usize, bytes.len());
            self.queue.write_buffer(target, offset, bytes);
        });

        #[cfg(not(target_arch = "wasm32"))]
        self.queue.write_buffer(target, offset, bytes);
    }

    /// Uploads an RGBA8 atlas into GPU texture memory and retains its view for
    /// the lifetime of this renderer. Queue::write_texture accepts tightly
    /// packed rows, so no 256-byte row padding or JS Array serialization is
    /// required for this path.
    pub fn upload_texture_atlas(&mut self, atlas: &TextureAtlas) -> Result<(), String> {
        let expected = (atlas.width as usize)
            .checked_mul(atlas.height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "Texture atlas dimensions overflow".to_owned())?;
        if atlas.width == 0 || atlas.height == 0 || atlas.pixels.len() != expected {
            return Err("Texture atlas has invalid RGBA8 dimensions".to_owned());
        }

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Minecraft resource texture atlas"),
            size: wgpu::Extent3d {
                width: atlas.width,
                height: atlas.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &atlas.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas.width * 4),
                rows_per_image: Some(atlas.height),
            },
            wgpu::Extent3d {
                width: atlas.width,
                height: atlas.height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.texture_atlases.push(GpuTextureAtlas {
            _texture: texture,
            _view: view,
            width: atlas.width,
            height: atlas.height,
        });
        Ok(())
    }

    pub fn texture_atlas_count(&self) -> usize {
        self.texture_atlases.len()
    }

    pub fn texture_atlas_dimensions(&self, index: usize) -> Option<[u32; 2]> {
        self.texture_atlases
            .get(index)
            .map(|atlas| [atlas.width, atlas.height])
    }

    /// Acquires, clears and presents one frame. World mesh draw submission is
    /// added separately so simulation and upload scheduling stay independent.
    pub fn render_frame(&mut self) -> Result<(), String> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout => {
                return Err("Timed out while waiting for the next canvas frame".to_owned());
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("GPU validation failed while acquiring the canvas frame".to_owned());
            }
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder =
            self.device
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
                multiview_mask: None,
            });
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        Ok(())
    }
}

/// Creates a short-lived JS typed-array view over the source's exact byte range.
/// The view must not be retained across calls which can grow Wasm memory.
#[cfg(target_arch = "wasm32")]
fn with_wasm_memory_view<R>(bytes: &[u8], upload: impl FnOnce(&Uint8Array) -> R) -> R {
    let memory: js_sys::WebAssembly::Memory = wasm_bindgen::memory().unchecked_into();
    let memory_buffer = memory.buffer();
    let full_memory_view = Uint8Array::new(&memory_buffer);
    let start = bytes.as_ptr() as usize;
    let end = start
        .checked_add(bytes.len())
        .expect("Wasm byte-slice address overflow");
    let start = u32::try_from(start).expect("Wasm linear-memory offset exceeds u32");
    let end = u32::try_from(end).expect("Wasm linear-memory end exceeds u32");
    let view = full_memory_view.subarray(start, end);
    upload(&view)
}
