use eagler_assets::TextureAtlas;
use js_sys::Uint8Array;
use web_sys::HtmlCanvasElement;
use wasm_bindgen::{JsCast, UnwrapThrowExt};

use crate::ChunkMesh;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    uv: [f32; 2],
}

struct GpuTextureAtlas {
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    _sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
}

/// Owns a browser surface, a textured voxel pipeline, reusable geometry buffers,
/// depth target, and the active Minecraft texture atlas.
pub struct GpuRenderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    backend_name: String,
    pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    texture_atlas: Option<GpuTextureAtlas>,
    depth_texture: wgpu::Texture,
    depth_view: wgpu::TextureView,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    vertex_capacity: u64,
    index_capacity: u64,
    index_count: u32,
    vertex_scratch: Vec<GpuVertex>,
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
        let mut instance_descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = backends;
        let instance = wgpu::Instance::new(instance_descriptor);
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|error| format!("Could not create browser GPU surface: {error}"))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
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

        let camera_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Voxel camera uniforms"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Minecraft resource atlas bindings"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let camera_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Camera view-projection matrix"),
            size: 64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Camera uniform bind group"),
            layout: &camera_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Textured voxel shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/voxel.wgsl").into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Textured voxel pipeline layout"),
            bind_group_layouts: &[&camera_bind_group_layout, &texture_bind_group_layout],
            push_constant_ranges: &[],
        });
        let vertex_attributes = wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x2
        ];
        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<GpuVertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &vertex_attributes,
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Textured voxel world pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[vertex_layout],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let (depth_texture, depth_view) = create_depth_target(&device, config.width, config.height);
        let vertex_capacity = 4;
        let index_capacity = 4;
        let vertex_buffer = create_buffer(&device, "Dynamic world vertices", vertex_capacity, wgpu::BufferUsages::VERTEX);
        let index_buffer = create_buffer(&device, "Dynamic world indices", index_capacity, wgpu::BufferUsages::INDEX);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            backend_name: format!("{:?}", adapter_info.backend),
            pipeline,
            camera_buffer,
            camera_bind_group,
            texture_bind_group_layout,
            texture_atlas: None,
            depth_texture,
            depth_view,
            vertex_buffer,
            index_buffer,
            vertex_capacity,
            index_capacity,
            index_count: 0,
            vertex_scratch: Vec::new(),
        })
    }

    pub fn backend_name(&self) -> &str {
        &self.backend_name
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
        let (texture, view) = create_depth_target(&self.device, self.config.width, self.config.height);
        self.depth_texture = texture;
        self.depth_view = view;
    }

    /// Uploads an atlas with a direct Rust byte-slice to WGPU queue transfer.
    /// WGPU performs the required device transfer; no intermediate JS number
    /// array or per-texel serialization is created.
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
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Pixel-art atlas sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Minecraft atlas bind group"),
            layout: &self.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        self.texture_atlas = Some(GpuTextureAtlas {
            _texture: texture,
            _view: view,
            _sampler: sampler,
            bind_group,
        });
        Ok(())
    }

    /// Updates the world camera in one small fixed-size uniform write.
    pub fn set_camera(&self, eye: [f32; 3], forward: [f32; 3], aspect: f32) {
        let matrix = view_projection(eye, forward, aspect.max(0.01));
        self.queue.write_buffer(&self.camera_buffer, 0, bytemuck::cast_slice(&matrix));
    }

    /// Reuses GPU allocations and the CPU conversion vector when a chunk mesh
    /// is refreshed. Indices are uploaded directly from their Rust slice.
    pub fn set_mesh(&mut self, mesh: &ChunkMesh) -> Result<(), String> {
        self.vertex_scratch.clear();
        self.vertex_scratch.extend(mesh.vertices.iter().map(|vertex| GpuVertex {
            position: vertex.position,
            uv: vertex.uv,
        }));
        let vertex_bytes = bytemuck::cast_slice(&self.vertex_scratch);
        let index_bytes = bytemuck::cast_slice(&mesh.indices);

        let required_vertex_capacity = vertex_bytes.len().max(4) as u64;
        if required_vertex_capacity > self.vertex_capacity {
            self.vertex_capacity = required_vertex_capacity.next_power_of_two();
            self.vertex_buffer = create_buffer(
                &self.device,
                "Dynamic world vertices",
                self.vertex_capacity,
                wgpu::BufferUsages::VERTEX,
            );
        }
        if !vertex_bytes.is_empty() {
            self.upload_buffer_bytes(&self.vertex_buffer, 0, vertex_bytes);
        }

        let required_index_capacity = index_bytes.len().max(4) as u64;
        if required_index_capacity > self.index_capacity {
            self.index_capacity = required_index_capacity.next_power_of_two();
            self.index_buffer = create_buffer(
                &self.device,
                "Dynamic world indices",
                self.index_capacity,
                wgpu::BufferUsages::INDEX,
            );
        }
        if !index_bytes.is_empty() {
            self.upload_buffer_bytes(&self.index_buffer, 0, index_bytes);
        }

        self.index_count = u32::try_from(mesh.indices.len())
            .map_err(|_| "World mesh contains more than u32::MAX indices".to_owned())?;
        Ok(())
    }

    pub fn texture_atlas_count(&self) -> usize {
        usize::from(self.texture_atlas.is_some())
    }

    /// Creates a short-lived JS typed-array view over this byte slice's exact
    /// WebAssembly linear-memory range. The view cannot outlive this call.
    pub fn upload_buffer_bytes(&self, target: &wgpu::Buffer, offset: u64, bytes: &[u8]) {
        #[cfg(target_arch = "wasm32")]
        with_wasm_memory_view(bytes, |view| {
            debug_assert_eq!(view.byte_length() as usize, bytes.len());
            self.queue.write_buffer(target, offset, bytes);
        });

        #[cfg(not(target_arch = "wasm32"))]
        self.queue.write_buffer(target, offset, bytes);
    }

    pub fn render_frame(&mut self) -> Result<(), String> {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout => {
                return Err("Timed out while waiting for the next canvas frame".to_owned());
            }
            wgpu::CurrentSurfaceTexture::Occluded => return Ok(()),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
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
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Eaglercraft world frame encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Textured world render pass"),
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
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            if self.index_count > 0 {
                if let Some(atlas) = &self.texture_atlas {
                    pass.set_pipeline(&self.pipeline);
                    pass.set_bind_group(0, &self.camera_bind_group, &[]);
                    pass.set_bind_group(1, &atlas.bind_group, &[]);
                    pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                    pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                    pass.draw_indexed(0..self.index_count, 0, 0..1);
                }
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        Ok(())
    }
}

fn create_buffer(
    device: &wgpu::Device,
    label: &'static str,
    capacity: u64,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: capacity.max(4),
        usage: usage | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn create_depth_target(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("World depth buffer"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn view_projection(eye: [f32; 3], forward: [f32; 3], aspect: f32) -> [f32; 16] {
    let normalize = |value: [f32; 3]| {
        let length = (value[0] * value[0] + value[1] * value[1] + value[2] * value[2])
            .sqrt()
            .max(1.0e-6);
        [value[0] / length, value[1] / length, value[2] / length]
    };
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let f = normalize(forward);
    let right = normalize(cross([0.0, 1.0, 0.0], f));
    let up = cross(f, right);

    let view = [
        right[0], up[0], -f[0], 0.0,
        right[1], up[1], -f[1], 0.0,
        right[2], up[2], -f[2], 0.0,
        -dot(right, eye), -dot(up, eye), dot(f, eye), 1.0,
    ];
    let fov = 70.0_f32.to_radians();
    let focal = 1.0 / (fov * 0.5).tan();
    let near = 0.05;
    let far = 512.0;
    let projection = [
        focal / aspect, 0.0, 0.0, 0.0,
        0.0, focal, 0.0, 0.0,
        0.0, 0.0, far / (near - far), -1.0,
        0.0, 0.0, (near * far) / (near - far), 0.0,
    ];

    let mut output = [0.0; 16];
    for column in 0..4 {
        for row in 0..4 {
            output[column * 4 + row] = (0..4)
                .map(|index| projection[index * 4 + row] * view[column * 4 + index])
                .sum();
        }
    }
    output
}

/// Short-lived Uint8Array view into wasm32 linear memory. It is used to check
/// the pointer/length passed into the WGPU upload path; the WGPU backend itself
/// receives the borrowed Rust slice and carries out the required GPU transfer.
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
