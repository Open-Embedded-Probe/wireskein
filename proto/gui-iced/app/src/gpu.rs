//! The waveform's shapes on the GPU: iced's shader primitive (iced_wgpu draws it inside the widget's bounds, the render
//! pass's viewport set to them) with prototype 2's instances and shader (proto/gui-web/web/src/gpu.ts): every shape
//! one instance of 8 floats [x0, y0, x1, y1, color, alpha, kind, width] (kind 0 a rectangle, 1 a segment `width` px
//! thick) in logical px of the widget, colors as indices into a palette of 16. One draw a view.

use iced::Rectangle;
use iced::wgpu;
use iced::widget::shader::{self, Viewport};
use std::collections::HashMap;
use std::sync::Arc;

const WGSL: &str = r#"
struct U { size: vec2f, pad: vec2f, pal: array<vec4f, 16> };
@group(0) @binding(0) var<uniform> u: U;
struct O { @builtin(position) p: vec4f, @location(0) c: vec4f };
@vertex fn vs(@builtin(vertex_index) vi: u32, @location(0) a: vec4f, @location(1) b: vec4f) -> O {
  var corners = array<vec2f, 6>(vec2f(0., 0.), vec2f(1., 0.), vec2f(0., 1.), vec2f(0., 1.), vec2f(1., 0.), vec2f(1., 1.));
  let k = corners[vi];
  var pos: vec2f;
  if (b.z < 0.5) {
    pos = mix(a.xy, a.zw, k);
  } else {
    let d = a.zw - a.xy;
    let len = max(length(d), 1e-3);
    let nrm = vec2f(-d.y, d.x) / len * (b.w * 0.5);
    pos = mix(a.xy, a.zw, k.x) + nrm * (k.y * 2. - 1.);
  }
  let ndc = vec2f(pos.x / u.size.x * 2. - 1., 1. - pos.y / u.size.y * 2.);
  let c = u.pal[u32(b.x)];
  let al = c.a * b.y;
  var o: O;
  o.p = vec4f(ndc, 0., 1.);
  o.c = vec4f(c.rgb * al, al);
  return o;
}
@fragment fn fs(o: O) -> @location(0) vec4f { return o.c; }
"#;

/// One view's frame: its instances (8 floats each) and palette; `key` tells the views apart (each its own buffers).
#[derive(Debug)]
pub struct WavePrim {
    pub key: u64,
    pub inst: Arc<Vec<f32>>,
    pub pal: [[f32; 4]; 16],
}

struct Slot {
    uniform: wgpu::Buffer,
    inst: wgpu::Buffer,
    cap: u64,
    bind: wgpu::BindGroup,
    n: u32,
    used: bool,
}

pub struct WavePipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    slots: HashMap<u64, Slot>,
}

impl shader::Pipeline for WavePipeline {
    fn new(device: &wgpu::Device, _queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("wsk wave"),
            source: wgpu::ShaderSource::Wgsl(WGSL.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("wsk wave"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            }],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("wsk wave"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let attrs = [
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 0, shader_location: 0 },
            wgpu::VertexAttribute { format: wgpu::VertexFormat::Float32x4, offset: 16, shader_location: 1 },
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("wsk wave"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout { array_stride: 32, step_mode: wgpu::VertexStepMode::Instance, attributes: &attrs }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });
        WavePipeline { pipeline, layout, slots: HashMap::new() }
    }
    fn trim(&mut self) {
        // views gone (a pane closed) give their buffers back
        self.slots.retain(|_, s| std::mem::take(&mut s.used));
    }
}

impl shader::Primitive for WavePrim {
    type Pipeline = WavePipeline;

    fn prepare(&self, p: &mut WavePipeline, device: &wgpu::Device, queue: &wgpu::Queue, bounds: &Rectangle, _viewport: &Viewport) {
        let bytes: &[u8] = bytemuck::cast_slice(&self.inst[..]);
        let need = (bytes.len() as u64).max(32);
        let make = |cap: u64| {
            let uniform = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wsk wave u"),
                size: 16 + 16 * 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let inst = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("wsk wave inst"),
                size: cap,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("wsk wave"),
                layout: &p.layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }],
            });
            Slot { uniform, inst, cap, bind, n: 0, used: true }
        };
        if p.slots.get(&self.key).is_none_or(|s| s.cap < need) {
            let cap = need.next_power_of_two().max(1 << 16);
            let s = make(cap);
            p.slots.insert(self.key, s);
        }
        let s = p.slots.get_mut(&self.key).unwrap();
        let mut u = [0f32; 4 + 64];
        u[0] = bounds.width;
        u[1] = bounds.height;
        for (i, c) in self.pal.iter().enumerate() {
            u[4 + i * 4..8 + i * 4].copy_from_slice(c);
        }
        queue.write_buffer(&s.uniform, 0, bytemuck::cast_slice(&u));
        if !bytes.is_empty() {
            queue.write_buffer(&s.inst, 0, bytes);
        }
        s.n = (self.inst.len() / 8) as u32;
        s.used = true;
    }

    fn draw(&self, p: &WavePipeline, pass: &mut wgpu::RenderPass<'_>) -> bool {
        let Some(s) = p.slots.get(&self.key) else { return true };
        if s.n > 0 {
            pass.set_pipeline(&p.pipeline);
            pass.set_bind_group(0, &s.bind, &[]);
            pass.set_vertex_buffer(0, s.inst.slice(..s.n as u64 * 32));
            pass.draw(0..6, 0..s.n);
        }
        true
    }
}
