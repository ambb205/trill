//! The graphics card's part: drawing the three layers of a [`Picture`] and
//! running the passes over them.
//!
//! The window's own renderer draws the interface straight to the screen
//! and has no place for a pass over what it drew. The stage therefore
//! draws its interface itself, into textures of its own, and what the
//! window (or a film) gets is the finished picture. The passes, in order:
//!
//! 1. The interface, its light and its focus are drawn as the kit painted
//!    them, blended the way the window blends: before anything else is
//!    done, the interface is the window's, pixel for pixel.
//! 2. *Combine*: interface and light become one picture in linear light
//!    (so that light adds up as light does), with the punch of a kick and
//!    the tear of a snare applied, and each pixel's focus kept beside it.
//! 3. *Focus*: what is out of focus is blurred, at half size, by gathering
//!    a disc of samples whose radius is the pixel's own blur. A sample is
//!    spread no further than its own blur reaches, so what is sharp does
//!    not smear into what is soft beside it.
//! 4. *Bloom*: the light alone is halved again and again and added back
//!    up the chain, which spreads it wide for the cost of a few small
//!    pictures.
//! 5. *Composite*: the three together are added into the frame being
//!    exposed. A film exposes a frame from several moments (motion blur);
//!    the window from one.
//! 6. *Finish*: the exposed frame is developed: highlights roll off, the
//!    corners darken, grain is laid over it.
//!
//! A film may also be drawn larger than it is shown ([`Picture::oversample`]):
//! the composite then takes each pixel from four.
//!
//! Everything here runs on whatever device it is given: the window's, so
//! that the finished picture is a texture the window can show, or one of
//! its own ([`Gpu::headless`]) for a film made away from any window.

pub use egui_wgpu::wgpu;

use crate::picture::{Look, Picture};
use egui::epaint::{ClippedPrimitive, Primitive, Vertex};
use egui::{TextureId, TexturesDelta};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

/// What the interface is drawn into: as the window draws it.
const DRAWN: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// What the passes work in: linear light, with room above white.
const LINEAR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// How many times the light is halved for its bloom.
const BLOOM_LEVELS: usize = 7;
/// A level of the bloom is at least this many pixels on its short side.
const BLOOM_SMALLEST: u32 = 6;
/// How long to wait for the graphics card before calling it stuck.
const PATIENCE: Duration = Duration::from_secs(20);

/// A graphics device to draw on.
#[derive(Clone)]
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// What the adapter calls itself.
    pub name: String,
    /// The first thing the device complained of, where that is listened
    /// for: on a device of the stage's own.
    problem: Arc<Mutex<Option<String>>>,
}

impl Gpu {
    /// A device that is already there: the window's.
    pub fn on(device: wgpu::Device, queue: wgpu::Queue, name: impl Into<String>) -> Self {
        Self { device, queue, name: name.into(), problem: Arc::default() }
    }

    /// A device of the stage's own, with no window: the machine's most
    /// capable graphics card. The error is a sentence.
    pub fn headless() -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            flags: wgpu::InstanceFlags::empty(),
            backend_options: wgpu::BackendOptions::from_env_or_default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let options = wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        };
        let adapter = pollster::block_on(instance.request_adapter(&options))
            .map_err(|error| format!("No graphics card could be used to draw the stage: {error}"))?;
        let info = adapter.get_info();
        let descriptor = wgpu::DeviceDescriptor { label: Some("trill stage"), ..Default::default() };
        let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
            .map_err(|error| format!("The graphics card ({}) could not be opened: {error}", info.name))?;
        let gpu = Self { device, queue, name: info.name, problem: Arc::default() };
        // A mistake in a pass, or a picture too large for the card, would
        // otherwise end the program: here it is kept and told.
        let slot = gpu.problem.clone();
        gpu.device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
            let mut slot = slot.lock();
            if slot.is_none() {
                *slot = Some(error.to_string());
            }
        }));
        Ok(gpu)
    }

    /// What the device has complained of since it was last asked, if
    /// anything.
    pub fn take_problem(&self) -> Option<String> {
        self.problem.lock().take()
    }
}

/// How much work is put into a picture.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Quality {
    /// Samples of the blur of what is out of focus.
    pub blur_samples: u32,
}

impl Quality {
    /// For the window, sixty times a second.
    pub const LIVE: Self = Self { blur_samples: 28 };
    /// For a film, which waits for no one.
    pub const FILM: Self = Self { blur_samples: 72 };
}

/// Draws pictures of the stage.
pub struct Renderer {
    gpu: Gpu,
    quality: Quality,
    meshes: Meshes,
    passes: Passes,
    targets: Option<Targets>,
    /// The buffer finished pictures are read back through, and its size.
    reading: Option<(wgpu::Buffer, u64)>,
}

impl Renderer {
    pub fn new(gpu: Gpu, quality: Quality) -> Self {
        let meshes = Meshes::new(&gpu.device);
        let passes = Passes::new(&gpu.device);
        Self { gpu, quality, meshes, passes, targets: None, reading: None }
    }

    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// Draws `picture` and adds it to the frame being exposed: `weight` of
    /// the frame comes from it, and with `first` the frame starts over.
    /// One call with a weight of 1 exposes a frame from one moment.
    pub fn expose(&mut self, picture: &Picture, weight: f32, first: bool) {
        let (device, queue) = (&self.gpu.device, &self.gpu.queue);
        picture.textures.hand_over(|delta| self.meshes.update_textures(device, queue, delta));
        let drawn = picture.drawn_size();
        let fits = self.targets.as_ref().is_some_and(|targets| targets.fits(picture));
        if !fits {
            self.targets = Some(Targets::new(device, &self.passes, picture));
        }
        let Some(targets) = &self.targets else { return };

        let scale = picture.drawn_pixels_per_point();
        let draws = self.meshes.upload(device, queue, [&picture.base, &picture.light, &picture.focus]);
        let points = [drawn[0] as f32 / scale, drawn[1] as f32 / scale, 0.0, 0.0];
        queue.write_buffer(&self.meshes.screen, 0, bytemuck::cast_slice(&points));
        let globals = Globals::of(&picture.look, picture, self.quality);
        queue.write_buffer(&self.passes.globals, 0, bytemuck::bytes_of(&globals));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("stage") });
        let background = picture.background;
        let clear = wgpu::Color {
            r: background.r() as f64 / 255.0,
            g: background.g() as f64 / 255.0,
            b: background.b() as f64 / 255.0,
            a: 1.0,
        };
        // The three layers. The focus is drawn at half size: it is soft
        // by nature.
        let layers = [
            (&targets.base, clear, scale),
            (&targets.light, wgpu::Color::BLACK, scale),
            (&targets.soft, wgpu::Color::BLACK, scale / 2.0),
        ];
        for ((target, clear, scale), draws) in layers.into_iter().zip(&draws) {
            let mut pass = begin(&mut encoder, &target.view, Some(clear));
            self.meshes.paint(&mut pass, draws, scale, target.size);
        }
        let passes = &self.passes;
        let run = |encoder: &mut wgpu::CommandEncoder,
                   target: &Target,
                   pipeline: &wgpu::RenderPipeline,
                   group: &wgpu::BindGroup,
                   clear: bool| {
            let mut pass = begin(encoder, &target.view, clear.then_some(wgpu::Color::BLACK));
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &passes.globals_group, &[]);
            pass.set_bind_group(1, group, &[]);
            pass.draw(0..3, 0..1);
        };
        run(&mut encoder, &targets.combined, &passes.combine, &targets.combine_group, true);
        run(&mut encoder, &targets.blurred, &passes.focus, &targets.focus_group, true);
        // The bloom: down the chain, then back up it, each level added to
        // the one above.
        run(&mut encoder, &targets.bloom[0], &passes.bloom_first, &targets.bloom_first_group, true);
        for level in 1..targets.bloom.len() {
            run(&mut encoder, &targets.bloom[level], &passes.bloom_down, &targets.bloom_groups[level - 1], true);
        }
        for level in (0..targets.bloom.len() - 1).rev() {
            run(&mut encoder, &targets.bloom[level], &passes.bloom_up, &targets.bloom_groups[level + 1], false);
        }
        {
            let mut pass = begin(&mut encoder, &targets.exposed.view, first.then_some(wgpu::Color::TRANSPARENT));
            pass.set_pipeline(&passes.composite);
            let weight = weight as f64;
            pass.set_blend_constant(wgpu::Color { r: weight, g: weight, b: weight, a: weight });
            pass.set_bind_group(0, &passes.globals_group, &[]);
            pass.set_bind_group(1, &targets.composite_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);
    }

    /// Develops the exposed frame into the finished picture.
    pub fn finish(&mut self) {
        let Some(targets) = &self.targets else { return };
        let mut encoder =
            self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("stage finish") });
        {
            let mut pass = begin(&mut encoder, &targets.finished.view, Some(wgpu::Color::BLACK));
            pass.set_pipeline(&self.passes.finish);
            pass.set_bind_group(0, &self.passes.globals_group, &[]);
            pass.set_bind_group(1, &targets.finish_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.gpu.queue.submit([encoder.finish()]);
    }

    /// Draws `picture` as a frame of its own: exposed from the one moment
    /// and finished.
    pub fn draw(&mut self, picture: &Picture) {
        self.expose(picture, 1.0, true);
        self.finish();
    }

    /// The finished picture, as a texture on the device: what the window
    /// shows. `None` before the first picture.
    pub fn finished(&self) -> Option<&wgpu::TextureView> {
        self.targets.as_ref().map(|targets| &targets.finished.view)
    }

    /// The size of the finished picture, in pixels.
    pub fn finished_size(&self) -> Option<[u32; 2]> {
        self.targets.as_ref().map(|targets| targets.finished.size)
    }

    /// Reads the finished picture back: rows of red, green, blue and alpha
    /// bytes, top row first, into `pixels`. The error is a sentence.
    pub fn read(&mut self, pixels: &mut Vec<u8>) -> Result<[u32; 2], String> {
        let Some(targets) = &self.targets else { return Err("No picture has been drawn yet.".to_string()) };
        let (device, queue) = (&self.gpu.device, &self.gpu.queue);
        let [width, height] = targets.finished.size;
        let row = width * 4;
        let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let size = padded as u64 * height as u64;
        if self.reading.as_ref().is_none_or(|(_, has)| *has != size) {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stage read"),
                size,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.reading = Some((buffer, size));
        }
        let Some((buffer, _)) = &self.reading else { unreachable!("made just above") };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("stage read") });
        encoder.copy_texture_to_buffer(
            targets.finished.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: None },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        let submitted = queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| drop(sender.send(result)));
        device
            .poll(wgpu::PollType::Wait { submission_index: Some(submitted), timeout: Some(PATIENCE) })
            .map_err(|error| format!("The graphics card did not finish the picture: {error}"))?;
        if let Some(problem) = self.gpu.take_problem() {
            buffer.unmap();
            return Err(format!("The graphics card could not draw the picture: {problem}"));
        }
        match receiver.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(format!("The picture could not be read back: {error}")),
            Err(_) => return Err("The picture could not be read back: the graphics card gave no answer.".to_string()),
        }
        {
            let view = buffer
                .slice(..)
                .get_mapped_range()
                .map_err(|error| format!("The picture could not be read back: {error}"))?;
            pixels.clear();
            pixels.reserve((row * height) as usize);
            for line in view.chunks_exact(padded as usize) {
                pixels.extend_from_slice(&line[..row as usize]);
            }
        }
        buffer.unmap();
        Ok([width, height])
    }
}

/// Begins a pass that draws into `view`, cleared to `clear` first if one
/// is given.
fn begin<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    clear: Option<wgpu::Color>,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: clear.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        ..Default::default()
    })
}

// ---- the layers ------------------------------------------------------------

/// One mesh of a layer, as it lies in the shared buffers.
struct Draw {
    clip: egui::Rect,
    texture: TextureId,
    indices: Range<u32>,
    first_vertex: i32,
}

/// Draws what egui tessellated.
struct Meshes {
    pipeline: wgpu::RenderPipeline,
    /// The size of the picture in points.
    screen: wgpu::Buffer,
    screen_group: wgpu::BindGroup,
    texture_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    textures: HashMap<TextureId, (wgpu::Texture, wgpu::BindGroup)>,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
}

impl Meshes {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stage mesh"),
            source: wgpu::ShaderSource::Wgsl(include_str!("mesh.wgsl").into()),
        });
        let screen = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stage screen"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let screen_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stage screen"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::VERTEX)],
        });
        let screen_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stage screen"),
            layout: &screen_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: screen.as_entire_binding() }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stage mesh texture"),
            entries: &[texture_entry(0), sampler_entry(1)],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("stage mesh"),
            bind_group_layouts: &[Some(&screen_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        // As the window blends: colors arrive multiplied by their alpha.
        let blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("stage mesh"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertex"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Uint32],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fragment"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: DRAWN,
                    blend: Some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("stage mesh"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            pipeline,
            screen,
            screen_group,
            texture_layout,
            sampler,
            textures: HashMap::new(),
            vertices: vertex_buffer(device, 64 * 1024),
            indices: index_buffer(device, 64 * 1024),
        }
    }

    /// Takes in what changed in egui's textures: the font's, which grows
    /// as new letters are used.
    fn update_textures(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, delta: &TexturesDelta) {
        for (id, changes) in &delta.set {
            for change in changes {
                let egui::ImageData::Color(image) = &change.image;
                let size = wgpu::Extent3d {
                    width: image.width() as u32,
                    height: image.height() as u32,
                    depth_or_array_layers: 1,
                };
                let whole = change.pos.is_none();
                if whole {
                    let texture = device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("stage font"),
                        size,
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: DRAWN,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("stage font"),
                        layout: &self.texture_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::Sampler(&self.sampler),
                            },
                        ],
                    });
                    self.textures.insert(*id, (texture, group));
                }
                let Some((texture, _)) = self.textures.get(id) else { continue };
                let origin =
                    change.pos.map_or(wgpu::Origin3d::ZERO, |[x, y]| wgpu::Origin3d { x: x as u32, y: y as u32, z: 0 });
                queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin, aspect: wgpu::TextureAspect::All },
                    bytemuck::cast_slice(&image.pixels),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4 * size.width),
                        rows_per_image: Some(size.height),
                    },
                    size,
                );
            }
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
    }

    /// Puts the meshes of the three layers into the buffers, one after
    /// another, and says where each lies.
    fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers: [&[ClippedPrimitive]; 3],
    ) -> [Vec<Draw>; 3] {
        let mut vertices: Vec<Vertex> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        let draws = layers.map(|layer| {
            let mut draws = Vec::with_capacity(layer.len());
            for ClippedPrimitive { clip_rect, primitive } in layer {
                // The stage paints no callbacks of its own.
                let Primitive::Mesh(mesh) = primitive else { continue };
                if mesh.indices.is_empty() {
                    continue;
                }
                let first = indices.len() as u32;
                draws.push(Draw {
                    clip: *clip_rect,
                    texture: mesh.texture_id,
                    indices: first..first + mesh.indices.len() as u32,
                    first_vertex: vertices.len() as i32,
                });
                vertices.extend_from_slice(&mesh.vertices);
                indices.extend_from_slice(&mesh.indices);
            }
            draws
        });
        let (vertex_bytes, index_bytes): (&[u8], &[u8]) =
            (bytemuck::cast_slice(&vertices), bytemuck::cast_slice(&indices));
        if self.vertices.size() < vertex_bytes.len() as u64 {
            self.vertices = vertex_buffer(device, (vertex_bytes.len() as u64).next_power_of_two());
        }
        if self.indices.size() < index_bytes.len() as u64 {
            self.indices = index_buffer(device, (index_bytes.len() as u64).next_power_of_two());
        }
        queue.write_buffer(&self.vertices, 0, vertex_bytes);
        queue.write_buffer(&self.indices, 0, index_bytes);
        draws
    }

    /// Draws one layer into a pass on a target of `size` pixels, at
    /// `scale` pixels per point.
    fn paint(&self, pass: &mut wgpu::RenderPass<'_>, draws: &[Draw], scale: f32, size: [u32; 2]) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.screen_group, &[]);
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
        for draw in draws {
            let edge = |value: f32, most: u32| (value * scale).round().clamp(0.0, most as f32) as u32;
            let (left, top) = (edge(draw.clip.min.x, size[0]), edge(draw.clip.min.y, size[1]));
            let (right, bottom) = (edge(draw.clip.max.x, size[0]), edge(draw.clip.max.y, size[1]));
            if right <= left || bottom <= top {
                continue;
            }
            let Some((_, group)) = self.textures.get(&draw.texture) else { continue };
            pass.set_scissor_rect(left, top, right - left, bottom - top);
            pass.set_bind_group(1, group, &[]);
            pass.draw_indexed(draw.indices.clone(), draw.first_vertex, 0..1);
        }
    }
}

fn vertex_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("stage vertices"),
        size,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn index_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("stage indices"),
        size,
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn uniform_entry(binding: u32, visibility: wgpu::ShaderStages) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            multisampled: false,
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

// ---- the passes ------------------------------------------------------------

/// What every pass is told, as `post.wgsl` lays it out.
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    scene: [f32; 4],
    out: [f32; 4],
    light: [f32; 4],
    focus: [f32; 4],
    finish: [f32; 4],
    tint: [f32; 4],
}

impl Globals {
    fn of(look: &Look, picture: &Picture, quality: Quality) -> Self {
        let [width, height] = picture.drawn_size().map(|side| side as f32);
        let (out_width, out_height) = (picture.format.width as f32, picture.format.height as f32);
        // Lengths of the look are in points; the passes count pixels.
        let scale = picture.drawn_pixels_per_point();
        let spread = look.spread.clamp(0.0, 0.95);
        Self {
            scene: [width, height, 1.0 / width, 1.0 / height],
            out: [out_width, out_height, 1.0 / out_width, 1.0 / out_height],
            // Every level of the bloom adds to the one above it: taken
            // together they are this much brighter than one.
            light: [look.light, look.glow * (1.0 - spread), look.flare, look.exposure],
            focus: [look.blur * scale, quality.blur_samples as f32, look.punch, look.tear * scale],
            finish: [look.vignette, look.grain, (look.seed % 4096) as f32, spread],
            tint: [look.flare_color[0], look.flare_color[1], look.flare_color[2], 0.0],
        }
    }
}

struct Passes {
    globals: wgpu::Buffer,
    globals_group: wgpu::BindGroup,
    /// Bind group layouts for passes that read one texture, and three.
    one: wgpu::BindGroupLayout,
    three: wgpu::BindGroupLayout,
    combine: wgpu::RenderPipeline,
    focus: wgpu::RenderPipeline,
    bloom_first: wgpu::RenderPipeline,
    bloom_down: wgpu::RenderPipeline,
    bloom_up: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    finish: wgpu::RenderPipeline,
}

impl Passes {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("stage passes"),
            source: wgpu::ShaderSource::Wgsl(include_str!("post.wgsl").into()),
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stage globals"),
            size: size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("stage passes"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stage globals"),
            entries: &[uniform_entry(0, wgpu::ShaderStages::FRAGMENT), sampler_entry(1)],
        });
        let globals_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stage globals"),
            layout: &globals_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            ],
        });
        let one = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stage one texture"),
            entries: &[texture_entry(0)],
        });
        let three = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("stage three textures"),
            entries: &[texture_entry(0), texture_entry(1), texture_entry(2)],
        });
        let layout_of = |textures: &wgpu::BindGroupLayout| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("stage pass"),
                bind_group_layouts: &[Some(&globals_layout), Some(textures)],
                immediate_size: 0,
            })
        };
        let (reads_one, reads_three) = (layout_of(&one), layout_of(&three));
        let pipeline = |entry: &str, layout: &wgpu::PipelineLayout, format, blend: Option<wgpu::BlendState>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("cover"),
                    buffers: &[],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState { format, blend, write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let adds = |source: wgpu::BlendFactor| {
            let component = wgpu::BlendComponent {
                src_factor: source,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            };
            Some(wgpu::BlendState { color: component, alpha: component })
        };
        Self {
            combine: pipeline("combine", &reads_three, LINEAR, None),
            focus: pipeline("focus", &reads_one, LINEAR, None),
            bloom_first: pipeline("bloom_first", &reads_one, LINEAR, None),
            bloom_down: pipeline("bloom_down", &reads_one, LINEAR, None),
            bloom_up: pipeline("bloom_up", &reads_one, LINEAR, adds(wgpu::BlendFactor::One)),
            // Each moment of an exposure is added by its weight.
            composite: pipeline("composite", &reads_three, LINEAR, adds(wgpu::BlendFactor::Constant)),
            finish: pipeline("finish", &reads_one, DRAWN, None),
            globals,
            globals_group,
            one,
            three,
        }
    }
}

// ---- what is drawn into ----------------------------------------------------

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: [u32; 2],
}

impl Target {
    fn new(device: &wgpu::Device, label: &str, size: [u32; 2], format: wgpu::TextureFormat, read: bool) -> Self {
        let size = size.map(|side| side.max(1));
        let mut usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        if read {
            usage |= wgpu::TextureUsages::COPY_SRC;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view, size }
    }
}

/// The textures of one size of picture, and the groups that hand each
/// pass what it reads.
struct Targets {
    format: crate::picture::Format,
    oversample: u32,
    base: Target,
    light: Target,
    /// The focus layer.
    soft: Target,
    combined: Target,
    blurred: Target,
    bloom: Vec<Target>,
    /// The frame being exposed, at the size of the finished picture.
    exposed: Target,
    finished: Target,
    combine_group: wgpu::BindGroup,
    focus_group: wgpu::BindGroup,
    bloom_first_group: wgpu::BindGroup,
    /// For each level of the bloom, the group that reads it.
    bloom_groups: Vec<wgpu::BindGroup>,
    composite_group: wgpu::BindGroup,
    finish_group: wgpu::BindGroup,
}

impl Targets {
    fn fits(&self, picture: &Picture) -> bool {
        self.format == picture.format && self.oversample == picture.oversample
    }

    fn new(device: &wgpu::Device, passes: &Passes, picture: &Picture) -> Self {
        let drawn = picture.drawn_size();
        let half = drawn.map(|side| side.div_ceil(2));
        let out = [picture.format.width, picture.format.height];
        let base = Target::new(device, "stage interface", drawn, DRAWN, false);
        let light = Target::new(device, "stage light", drawn, DRAWN, false);
        let soft = Target::new(device, "stage focus", half, DRAWN, false);
        let combined = Target::new(device, "stage combined", drawn, LINEAR, false);
        let blurred = Target::new(device, "stage blurred", half, LINEAR, false);
        let mut bloom = Vec::new();
        let mut size = half;
        for _ in 0..BLOOM_LEVELS {
            bloom.push(Target::new(device, "stage bloom", size, LINEAR, false));
            size = size.map(|side| side.div_ceil(2));
            if size[0].min(size[1]) < BLOOM_SMALLEST {
                break;
            }
        }
        let exposed = Target::new(device, "stage exposed", out, LINEAR, false);
        let finished = Target::new(device, "stage finished", out, DRAWN, true);

        let reads = |layout: &wgpu::BindGroupLayout, targets: &[&Target]| {
            let entries: Vec<wgpu::BindGroupEntry<'_>> = targets
                .iter()
                .enumerate()
                .map(|(binding, target)| wgpu::BindGroupEntry {
                    binding: binding as u32,
                    resource: wgpu::BindingResource::TextureView(&target.view),
                })
                .collect();
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("stage pass"),
                layout,
                entries: &entries,
            })
        };
        Self {
            combine_group: reads(&passes.three, &[&base, &light, &soft]),
            focus_group: reads(&passes.one, &[&combined]),
            bloom_first_group: reads(&passes.one, &[&light]),
            bloom_groups: bloom.iter().map(|level| reads(&passes.one, &[level])).collect(),
            composite_group: reads(&passes.three, &[&combined, &blurred, &bloom[0]]),
            finish_group: reads(&passes.one, &[&exposed]),
            format: picture.format,
            oversample: picture.oversample,
            base,
            light,
            soft,
            combined,
            blurred,
            bloom,
            exposed,
            finished,
        }
    }
}
