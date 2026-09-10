//! B0 gate prototype: winit window + wgpu + cosmic-text glyphs + spring morph + IME.
//! Must run on a real desktop (GPU required): `cargo run -p flashagent-ui --bin b0 --release`.
//!
//! Gate criteria visible in this binary:
//! 1. Cyrillic text rendered via cosmic-text on a custom wgpu pipeline.
//! 2. IME preedit shown in accent color (type with a Cyrillic/CJK IME).
//! 3. Spring-morphing button (click it) — spring physics, not easing curves.
//! 4. Looping background animations: cursor blink, title breathing — parallel, never blocking.
//! 5. VSync: FPS follows monitor refresh rate.

use std::collections::HashMap;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use cosmic_text::{Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, SwashCache};
use pollster::block_on;
use swash::scale::image::Content;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

const ATLAS_W: u32 = 1024;
const ATLAS_H: u32 = 2048;

const COLOR_BG: [f32; 4] = [0.055, 0.06, 0.075, 1.0];
const COLOR_TITLE: [f32; 4] = [0.92, 0.92, 0.95, 1.0];
const COLOR_HINT: [f32; 4] = [0.55, 0.57, 0.62, 1.0];
const COLOR_BOX: [f32; 4] = [0.11, 0.115, 0.14, 1.0];
const COLOR_TEXT: [f32; 4] = [0.95, 0.95, 0.97, 1.0];
const COLOR_ACCENT: [f32; 4] = [0.55, 0.75, 1.0, 1.0];
const COLOR_BUTTON: [f32; 4] = [0.404, 0.314, 0.643, 1.0];

const SHADER: &str = r#"
struct VOut {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
    @location(1) color: vec4f,
};

@vertex
fn vs_main(@location(0) pos: vec2f, @location(1) uv: vec2f, @location(2) color: vec4f) -> VOut {
    var out: VOut;
    out.pos = vec4f(pos, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

@fragment
fn fs_main(in: VOut) -> @location(0) vec4f {
    let a = textureSample(tex, samp, in.uv).r;
    return vec4f(in.color.rgb, in.color.a * a);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    pos: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

/// Spring physics for the morph animation (semi-implicit Euler).
struct Spring {
    value: f32,
    velocity: f32,
    target: f32,
    stiffness: f32,
    damping: f32,
}

impl Spring {
    fn new() -> Self {
        Self { value: 1.0, velocity: 0.0, target: 1.0, stiffness: 170.0, damping: 14.0 }
    }

    fn step(&mut self, dt: f32) {
        let force = self.stiffness * (self.target - self.value) - self.damping * self.velocity;
        self.velocity += force * dt;
        self.value += self.velocity * dt;
    }
}

#[derive(Clone, Copy)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    left: i32,
    top: i32,
}

/// CPU-side RGBA glyph atlas with a row packer. Uploaded wholesale when dirty.
struct Atlas {
    data: Vec<u8>,
    cursor: (u32, u32),
    row_h: u32,
    map: HashMap<CacheKey, Rect>,
    dirty: bool,
}

impl Atlas {
    fn new() -> Self {
        let mut data = vec![0u8; (ATLAS_W * ATLAS_H * 4) as usize];
        // Solid white pixel at (0,0) for untextured quads.
        data[0] = 255;
        data[1] = 255;
        data[2] = 255;
        data[3] = 255;
        Self { data, cursor: (1, 0), row_h: 1, map: HashMap::new(), dirty: true }
    }

    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if self.cursor.0 + w > ATLAS_W {
            self.cursor = (0, self.cursor.1 + self.row_h);
            self.row_h = 0;
        }
        if self.cursor.1 + h > ATLAS_H {
            return None;
        }
        let pos = self.cursor;
        self.cursor.0 += w;
        self.row_h = self.row_h.max(h);
        Some(pos)
    }

    fn glyph(
        &mut self,
        font_system: &mut FontSystem,
        swash: &mut SwashCache,
        key: CacheKey,
    ) -> Option<Rect> {
        if let Some(rect) = self.map.get(&key) {
            return Some(*rect);
        }
        let img = swash.get_image_uncached(font_system, key)?;
        let w = img.placement.width;
        let h = img.placement.height;
        let (x, y) = self.alloc(w.max(1), h.max(1))?;
        match img.content {
            Content::Color => {
                for row in 0..h {
                    let src = (row * w * 4) as usize;
                    let dst = ((y + row) * ATLAS_W + x) as usize * 4;
                    let len = (w * 4) as usize;
                    self.data[dst..dst + len].copy_from_slice(&img.data[src..src + len]);
                }
            }
            _ => {
                for row in 0..h {
                    for col in 0..w {
                        let a = img.data[(row * w + col) as usize];
                        let dst = (((y + row) * ATLAS_W + x + col) as usize) * 4;
                        self.data[dst] = a;
                        self.data[dst + 1] = 0;
                        self.data[dst + 2] = 0;
                        self.data[dst + 3] = 255;
                    }
                }
            }
        }
        let rect = Rect {
            x,
            y,
            w: w.max(1),
            h: h.max(1),
            left: img.placement.left,
            top: img.placement.top,
        };
        self.map.insert(key, rect);
        self.dirty = true;
        Some(rect)
    }
}

struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    atlas_texture: wgpu::Texture,
}

struct App {
    window: Option<Window>,
    gpu: Option<Gpu>,
    configured: (u32, u32),
    font_system: FontSystem,
    swash: SwashCache,
    atlas: Atlas,
    composer: String,
    preedit: Option<String>,
    cursor_pos: (f32, f32),
    mods: winit::keyboard::ModifiersState,
    pressed: bool,
    spring: Spring,
    t: f32,
    last: Instant,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            gpu: None,
            configured: (0, 0),
            font_system: FontSystem::new(),
            swash: SwashCache::new(),
            atlas: Atlas::new(),
            composer: String::new(),
            preedit: None,
            cursor_pos: (0.0, 0.0),
            mods: winit::keyboard::ModifiersState::empty(),
            pressed: false,
            spring: Spring::new(),
            t: 0.0,
            last: Instant::now(),
        }
    }

    fn init_gpu(window: &Window) -> Option<Gpu> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window).ok()?;
        // SAFETY: the surface borrows the Window owned by App (field `window`,
        // declared before `gpu`, so it outlives the Gpu). The lifetime is erased
        // here and restored by the ownership relationship in App.
        let surface: wgpu::Surface<'static> = unsafe { std::mem::transmute(surface) };
        let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok()?;
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .ok()?;
        let caps = surface.get_capabilities(&adapter);
        // Values are authored in sRGB; pick an 8-bit non-sRGB format so the
        // compositor doesn't apply a second linear->sRGB conversion
        // (washed-out colors). 16-bit formats need extra device features.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
            .unwrap_or(caps.formats[0]);

        let atlas_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: wgpu::Extent3d { width: ATLAS_W, height: ATLAS_H, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
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
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&atlas_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quad"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quad layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 0, shader_location: 0 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x2, offset: 8, shader_location: 1 },
                        wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 2 },
                    ],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });

        Some(Gpu { surface, device, queue, format, pipeline, bind_group, atlas_texture })
    }

    fn configure(&mut self) {
        let Some(window) = &self.window else { return };
        let Some(gpu) = &self.gpu else { return };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 || self.configured == (size.width, size.height) {
            return;
        }
        gpu.surface.configure(
            &gpu.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: gpu.format,
                width: size.width,
                height: size.height,
                present_mode: wgpu::PresentMode::Fifo,
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                view_formats: vec![],
                desired_maximum_frame_latency: 2,
            },
        );
        self.configured = (size.width, size.height);
    }

    #[allow(clippy::too_many_arguments)]
    fn quad(
        vs: &mut Vec<Vertex>,
        screen: (f32, f32),
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        uv: [f32; 4],
        color: [f32; 4],
    ) {
        let x0 = x / screen.0 * 2.0 - 1.0;
        let x1 = (x + w) / screen.0 * 2.0 - 1.0;
        let y0 = 1.0 - y / screen.1 * 2.0;
        let y1 = 1.0 - (y + h) / screen.1 * 2.0;
        let u0 = uv[0] / ATLAS_W as f32;
        let v0 = uv[1] / ATLAS_H as f32;
        let u1 = (uv[0] + uv[2]) / ATLAS_W as f32;
        let v1 = (uv[1] + uv[3]) / ATLAS_H as f32;
        let c = color;
        vs.extend_from_slice(&[
            Vertex { pos: [x0, y0], uv: [u0, v0], color: c },
            Vertex { pos: [x0, y1], uv: [u0, v1], color: c },
            Vertex { pos: [x1, y1], uv: [u1, v1], color: c },
            Vertex { pos: [x0, y0], uv: [u0, v0], color: c },
            Vertex { pos: [x1, y1], uv: [u1, v1], color: c },
            Vertex { pos: [x1, y0], uv: [u1, v0], color: c },
        ]);
    }

    fn solid_quad(vs: &mut Vec<Vertex>, screen: (f32, f32), x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        Self::quad(vs, screen, x, y, w, h, [0.0, 0.0, 1.0, 1.0], color);
    }

    /// Shape and draw one text line; returns its advance width.
    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        font_system: &mut FontSystem,
        swash: &mut SwashCache,
        atlas: &mut Atlas,
        vs: &mut Vec<Vertex>,
        screen: (f32, f32),
        text: &str,
        font_size: f32,
        x: f32,
        y: f32,
        color: [f32; 4],
        mono: bool,
        max_w: Option<f32>,
    ) -> f32 {
        let metrics = Metrics::new(font_size, font_size * 1.4);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, max_w, None);
        let attrs = if mono {
            Attrs::new().family(Family::Monospace)
        } else {
            Attrs::new()
        };
        buffer.set_text(font_system, text, &attrs, Shaping::Advanced);
        buffer.shape_until_scroll(font_system, false);

        let mut width = 0.0f32;
        for run in buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let physical = glyph.physical((0.0, 0.0), 1.0);
                if let Some(rect) = atlas.glyph(font_system, swash, physical.cache_key)
                {
                    let gx = x + physical.x as f32 + rect.left as f32;
                    let gy = y + run.line_y + physical.y as f32 - rect.top as f32;
                    Self::quad(
                        vs,
                        screen,
                        gx,
                        gy,
                        rect.w as f32,
                        rect.h as f32,
                        [rect.x as f32, rect.y as f32, rect.w as f32, rect.h as f32],
                        color,
                    );
                }
            }
            width = width.max(run.line_w);
        }
        width
    }

    fn toggle_button(&mut self) {
        let (w, h) = self.screen();
        let dpr = self.dpr();
        let bw = 170.0 * dpr;
        let bh = 46.0 * dpr;
        let box_h = 52.0 * dpr;
        let box_y = h - box_h - 24.0 * dpr;
        let bx = w - bw - 24.0 * dpr;
        let by = box_y + (box_h - bh) / 2.0;
        let (cx, cy) = self.cursor_pos;
        if cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
            self.pressed = !self.pressed;
            self.spring.target = if self.pressed { 1.35 } else { 1.0 };
        }
    }

    fn dpr(&self) -> f32 {
        self.window.as_ref().map_or(1.0, |w| w.scale_factor() as f32)
    }

    fn screen(&self) -> (f32, f32) {
        self.window
            .as_ref()
            .map(|w| (w.inner_size().width as f32, w.inner_size().height as f32))
            .unwrap_or((1280.0, 800.0))
    }

    fn render(&mut self) {
        self.configure();

        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.05);
        self.last = now;
        self.t += dt;
        self.spring.step(dt);

        let screen = self.screen();
        let dpr = self.dpr();

        // Disjoint field borrows: gpu/window immutable, text state mutable.
        let Self { window, gpu, font_system, swash, atlas, composer, preedit, configured, .. } = self;
        let Some(gpu) = gpu.as_ref() else { return };
        let Some(window) = window.as_ref() else { return };

        let frame = match gpu.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::OutOfMemory) => {
                eprintln!("GPU out of memory, exiting");
                window.request_redraw();
                return;
            }
            Err(_) => {
                *configured = (0, 0);
                window.request_redraw();
                return;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());

        if atlas.dirty {
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &gpu.atlas_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &atlas.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(ATLAS_W * 4),
                    rows_per_image: Some(ATLAS_H),
                },
                wgpu::Extent3d { width: ATLAS_W, height: ATLAS_H, depth_or_array_layers: 1 },
            );
            atlas.dirty = false;
        }

        let mut vs: Vec<Vertex> = Vec::new();

        // Looping background animations (parallel, never blocking):
        let title_alpha = 0.75 + 0.25 * (self.t * 2.0).sin();
        let blink = (self.t * 2.2).fract() < 0.55;

        // Composer box.
        let box_h = 52.0 * dpr;
        let box_y = screen.1 - box_h - 24.0 * dpr;
        Self::solid_quad(&mut vs, screen, 16.0 * dpr, box_y, screen.0 - 32.0 * dpr, box_h, COLOR_BOX);

        // Spring-morphing button.
        let bw = 170.0 * dpr;
        let bh = 46.0 * dpr;
        let bx = screen.0 - bw - 24.0 * dpr;
        let by = box_y + (box_h - bh) / 2.0;
        let s = self.spring.value;
        let cbx = bx + bw / 2.0;
        let cby = by + bh / 2.0;
        Self::solid_quad(
            &mut vs,
            screen,
            cbx - bw * s / 2.0,
            cby - bh * s / 2.0,
            bw * s,
            bh * s,
            COLOR_BUTTON,
        );

        // Texts.
        let title = "FlashAgent · B0 · wgpu + cosmic-text";
        let mut color = COLOR_TITLE;
        color[3] = title_alpha;
        Self::draw_text(font_system, swash, atlas, &mut vs, screen, title, 20.0 * dpr, 24.0 * dpr, 24.0 * dpr, color, false, None);
        let hint = "Type (IME supported) · click button — spring morph · Enter — clear · Esc — exit";
        Self::draw_text(font_system, swash, atlas, &mut vs, screen, hint, 13.0 * dpr, 24.0 * dpr, 64.0 * dpr, COLOR_HINT, false, Some(screen.0 - 48.0 * dpr));

        // Composer + preedit + cursor.
        let text_x = 16.0 * dpr + 16.0 * dpr;
        let text_y = box_y + 8.0 * dpr;
        let max_w = Some(bx - text_x - 16.0 * dpr);
        let composer_w = Self::draw_text(font_system, swash, atlas, &mut vs, screen, composer, 17.0 * dpr, text_x, text_y, COLOR_TEXT, true, max_w);
        let preedit_w = match preedit {
            Some(p) if !p.is_empty() => {
                let w = Self::draw_text(font_system, swash, atlas, &mut vs, screen, p, 17.0 * dpr, text_x + composer_w, text_y, COLOR_ACCENT, true, max_w);
                // Preedit underline.
                Self::solid_quad(&mut vs, screen, text_x + composer_w, text_y + 24.0 * dpr, w, 1.5 * dpr, COLOR_ACCENT);
                w
            }
            _ => 0.0,
        };
        if blink {
            Self::solid_quad(
                &mut vs,
                screen,
                text_x + composer_w + preedit_w + 2.0,
                text_y + 2.0 * dpr,
                2.0 * dpr,
                20.0 * dpr,
                COLOR_ACCENT,
            );
        }

        // Button label (drawn after morph so it stays readable).
        let label_x = bx + (bw - 110.0 * dpr) / 2.0;
        Self::draw_text(font_system, swash, atlas, &mut vs, screen, "Send ⏎", 15.0 * dpr, label_x, by + 12.0 * dpr, COLOR_TEXT, false, None);

        // Upload and draw.
        let vbuf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("verts"),
            size: (vs.len() * std::mem::size_of::<Vertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue.write_buffer(&vbuf, 0, bytemuck::cast_slice(&vs));

        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("b0") });
        {
            let mut rpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("b0"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: COLOR_BG[0] as f64,
                            g: COLOR_BG[1] as f64,
                            b: COLOR_BG[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            rpass.set_pipeline(&gpu.pipeline);
            rpass.set_bind_group(0, &gpu.bind_group, &[]);
            rpass.set_vertex_buffer(0, vbuf.slice(..));
            rpass.draw(0..vs.len() as u32, 0..1);
        }
        gpu.queue.submit(Some(enc.finish()));
        frame.present();
        window.request_redraw();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let window = match event_loop.create_window(
            Window::default_attributes().with_title("FlashAgent B0 — Render Prototype"),
        ) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("window creation failed: {e}");
                event_loop.exit();
                return;
            }
        };
        self.gpu = Self::init_gpu(&window);
        if self.gpu.is_none() {
            eprintln!("no suitable GPU adapter found — run this on a desktop machine");
            event_loop.exit();
            return;
        }
        self.window = Some(window);
        self.last = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.configured = (0, 0);
                self.window.as_ref().map(Window::request_redraw);
            }
            WindowEvent::RedrawRequested => self.render(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_pos = (position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.toggle_button();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    match &event.logical_key {
                        Key::Named(NamedKey::Escape) => event_loop.exit(),
                        Key::Named(NamedKey::Enter) => {
                            self.composer.clear();
                            self.preedit = None;
                        }
                        Key::Named(NamedKey::Space) => self.composer.push(' '),
                        Key::Named(NamedKey::Backspace) => {
                            self.composer.pop();
                        }

                        Key::Character(s) if !self.mods.control_key() && !self.mods.super_key() => {
                            self.composer.push_str(s);
                        }
                        _ => {}
                    }
                }
            }
            WindowEvent::Ime(ime) => match ime {
                Ime::Preedit(s, _) => self.preedit = (!s.is_empty()).then_some(s),
                Ime::Commit(s) => {
                    self.composer.push_str(&s);
                    self.preedit = None;
                }
                Ime::Enabled | Ime::Disabled => {}
            },
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    let mut app = App::new();
    event_loop.run_app(&mut app)?;
    Ok(())
}
