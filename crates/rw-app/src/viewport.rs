//! 3D viewport: wgpu renderer (offscreen MSAA target shown as an egui image),
//! orbit camera with animated standard views, and CPU face picking.

use egui_wgpu::wgpu;
use glam::{Mat4, Quat, Vec3, Vec4};
use rw_core::kernel::Mesh;
use std::sync::Arc;
use wgpu::util::DeviceExt;

const MSAA: u32 = 4;
const COLOR_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const DEPTH_FMT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    pos: [f32; 3],
    nor: [f32; 3],
    face: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct LineVertex {
    pub pos: [f32; 3],
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    light: [f32; 4],
    eye: [f32; 4],
    base: [f32; 4],
    /// x = hovered face, y = selected face (u32::MAX = none)
    hl: [u32; 4],
}

const SHADER: &str = r#"
struct U { vp: mat4x4<f32>, light: vec4<f32>, eye: vec4<f32>, base: vec4<f32>, hl: vec4<u32> };
@group(0) @binding(0) var<uniform> u: U;

struct MO { @builtin(position) pos: vec4<f32>, @location(0) n: vec3<f32>, @location(1) @interpolate(flat) face: u32 };

@vertex fn vs_mesh(@location(0) p: vec3<f32>, @location(1) n: vec3<f32>, @location(2) face: u32) -> MO {
    var o: MO;
    o.pos = u.vp * vec4<f32>(p, 1.0);
    o.n = n;
    o.face = face;
    return o;
}

@fragment fn fs_mesh(v: MO, @builtin(front_facing) ff: bool) -> @location(0) vec4<f32> {
    var n = normalize(v.n);
    if (dot(n, u.eye.xyz) < 0.0) { n = -n; }
    let l = normalize(u.light.xyz);
    let diff = max(dot(n, l), 0.0);
    let h = normalize(l + u.eye.xyz);
    let spec = pow(max(dot(n, h), 0.0), 48.0) * 0.28;
    // soft hemispheric ambient
    let amb = 0.30 + 0.10 * n.y;
    var c = u.base.rgb;
    if (v.face == u.hl.y) { c = mix(c, vec3<f32>(0.25, 0.55, 1.0), 0.65); }
    else if (v.face == u.hl.x) { c = mix(c, vec3<f32>(1.0, 0.72, 0.25), 0.45); }
    let rgb = c * (amb + 0.68 * diff) + vec3<f32>(spec);
    return vec4<f32>(rgb, 1.0);
}

struct LO { @builtin(position) pos: vec4<f32>, @location(0) c: vec4<f32> };

@vertex fn vs_line(@location(0) p: vec3<f32>, @location(1) c: vec4<f32>) -> LO {
    var o: LO;
    var cp = u.vp * vec4<f32>(p, 1.0);
    // pull lines slightly toward the camera so edges win the depth test
    cp.z = cp.z - 0.0004 * cp.w;
    o.pos = cp;
    o.c = c;
    return o;
}

@fragment fn fs_line(v: LO) -> @location(0) vec4<f32> { return v.c; }
"#;

pub struct Camera {
    pub target: Vec3,
    pub rot: Quat, // camera-to-world rotation
    pub dist: f32,
    pub ortho: bool,
    anim: Option<(Quat, Quat, f32, Vec3, Vec3, f32, f32)>, // from, to, t, target from/to, dist from/to
}

impl Default for Camera {
    fn default() -> Self {
        Camera { target: Vec3::ZERO, rot: iso_rot(), dist: 250.0, ortho: true, anim: None }
    }
}

pub fn iso_rot() -> Quat {
    look_rot(Vec3::new(1.0, 0.8, 1.0), Vec3::Y)
}

/// Rotation whose +Z points along `eye_dir` (from target toward camera).
pub fn look_rot(eye_dir: Vec3, up: Vec3) -> Quat {
    let z = eye_dir.normalize();
    let x = up.cross(z).normalize();
    let y = z.cross(x);
    Quat::from_mat3(&glam::Mat3::from_cols(x, y, z)).normalize()
}

impl Camera {
    pub fn eye_dir(&self) -> Vec3 {
        self.rot * Vec3::Z
    }
    pub fn view(&self) -> Mat4 {
        let eye = self.target + self.eye_dir() * self.dist;
        Mat4::from_rotation_translation(self.rot, eye).inverse()
    }
    /// Right-handed projection with wgpu's 0..1 depth range.
    pub fn proj(&self, aspect: f32) -> Mat4 {
        use glam::Vec4 as V;
        if self.ortho {
            let h = self.dist * 0.42;
            let w = h * aspect;
            let (n, f) = (-self.dist * 20.0, self.dist * 20.0);
            let a = 1.0 / (n - f);
            Mat4::from_cols(V::new(1.0 / w, 0.0, 0.0, 0.0), V::new(0.0, 1.0 / h, 0.0, 0.0), V::new(0.0, 0.0, a, 0.0), V::new(0.0, 0.0, n * a, 1.0))
        } else {
            let (n, f) = (self.dist * 0.002, self.dist * 20.0);
            let t = 1.0 / (40f32.to_radians() * 0.5).tan();
            Mat4::from_cols(
                V::new(t / aspect, 0.0, 0.0, 0.0),
                V::new(0.0, t, 0.0, 0.0),
                V::new(0.0, 0.0, f / (n - f), -1.0),
                V::new(0.0, 0.0, n * f / (n - f), 0.0),
            )
        }
    }
    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.anim = None;
        // Turntable around world Y plus pitch about the camera's right axis.
        let yaw = Quat::from_axis_angle(Vec3::Y, -dx * 0.008);
        let right = self.rot * Vec3::X;
        let pitch = Quat::from_axis_angle(right, -dy * 0.008);
        self.rot = (yaw * pitch * self.rot).normalize();
    }
    pub fn pan(&mut self, dx: f32, dy: f32, viewport_h: f32) {
        self.anim = None;
        let s = self.dist * 0.84 / viewport_h.max(1.0);
        self.target += self.rot * Vec3::new(-dx * s, dy * s, 0.0);
    }
    pub fn zoom(&mut self, factor: f32, toward: Option<Vec3>) {
        self.anim = None;
        let nd = (self.dist * factor).clamp(0.01, 1e7);
        if let Some(p) = toward {
            // keep the point under the cursor fixed
            let k = 1.0 - nd / self.dist;
            let d = p - self.target;
            let side = d - self.eye_dir() * d.dot(self.eye_dir());
            self.target += side * k;
        }
        self.dist = nd;
    }
    pub fn animate_to(&mut self, rot: Quat, target: Vec3, dist: f32) {
        self.anim = Some((self.rot, rot, 0.0, self.target, target, self.dist, dist));
    }
    /// Advance animation; returns true while animating.
    pub fn tick(&mut self, dt: f32) -> bool {
        if let Some((r0, r1, t, a0, a1, d0, d1)) = self.anim {
            let t = (t + dt / 0.35).min(1.0);
            let e = t * t * (3.0 - 2.0 * t);
            self.rot = r0.slerp(r1, e).normalize();
            self.target = a0.lerp(a1, e);
            self.dist = d0 + (d1 - d0) * e;
            self.anim = if t >= 1.0 { None } else { Some((r0, r1, t, a0, a1, d0, d1)) };
            return true;
        }
        false
    }
    /// World point → viewport pixel (None if behind the camera).
    pub fn project(&self, p: Vec3, w: f32, h: f32) -> Option<glam::Vec2> {
        let c = self.proj(w / h) * self.view() * p.extend(1.0);
        if c.w <= 1e-9 {
            return None;
        }
        let n = c.truncate() / c.w;
        Some(glam::Vec2::new((n.x + 1.0) * 0.5 * w, (1.0 - n.y) * 0.5 * h))
    }

    /// World-space ray through a viewport pixel (0..w, 0..h).
    pub fn ray(&self, px: f32, py: f32, w: f32, h: f32) -> (Vec3, Vec3) {
        let inv = (self.proj(w / h) * self.view()).inverse();
        let x = px / w * 2.0 - 1.0;
        let y = 1.0 - py / h * 2.0;
        let p0 = inv * Vec4::new(x, y, 0.0, 1.0);
        let p1 = inv * Vec4::new(x, y, 1.0, 1.0);
        let a = p0.truncate() / p0.w;
        let b = p1.truncate() / p1.w;
        (a, (b - a).normalize())
    }
}

struct Targets {
    size: [u32; 2],
    msaa: wgpu::TextureView,
    resolve: wgpu::Texture,
    resolve_view: wgpu::TextureView,
    depth: wgpu::TextureView,
}

pub struct Viewport {
    pub cam: Camera,
    pub hovered: Option<u32>,
    pub selected_face: Option<u32>,
    pub show_edges: bool,
    mesh_pipe: wgpu::RenderPipeline,
    line_pipe: wgpu::RenderPipeline,
    ubuf: wgpu::Buffer,
    bind: wgpu::BindGroup,
    vbuf: Option<(wgpu::Buffer, wgpu::Buffer, u32)>,
    ebuf: Option<(wgpu::Buffer, u32)>,
    overlay: Option<(wgpu::Buffer, u32)>,
    targets: Option<Targets>,
    pub tex_id: Option<egui::TextureId>,
    mesh: Option<Arc<Mesh>>,
    bounds: Option<(Vec3, Vec3)>,
}

impl Viewport {
    pub fn new(rs: &egui_wgpu::RenderState) -> Viewport {
        let dev = &rs.device;
        let shader = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rw-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let ubuf = dev.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rw-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = dev.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind = dev.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() }],
        });
        let layout = dev.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let depth = |write: bool| wgpu::DepthStencilState {
            format: DEPTH_FMT,
            depth_write_enabled: Some(write),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: Default::default(),
            bias: Default::default(),
        };
        let ms = wgpu::MultisampleState { count: MSAA, mask: !0, alpha_to_coverage_enabled: false };
        let target = [Some(wgpu::ColorTargetState {
            format: COLOR_FMT,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let mesh_pipe = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rw-mesh"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_mesh"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Uint32],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_mesh"),
                compilation_options: Default::default(),
                targets: &target,
            }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(depth(true)),
            multisample: ms,
            multiview_mask: None,
            cache: None,
        });
        let line_pipe = dev.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rw-lines"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_line"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<LineVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_line"),
                compilation_options: Default::default(),
                targets: &target,
            }),
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::LineList, ..Default::default() },
            depth_stencil: Some(depth(false)),
            multisample: ms,
            multiview_mask: None,
            cache: None,
        });
        Viewport {
            cam: Camera::default(),
            hovered: None,
            selected_face: None,
            show_edges: true,
            mesh_pipe,
            line_pipe,
            ubuf,
            bind,
            vbuf: None,
            ebuf: None,
            overlay: None,
            targets: None,
            tex_id: None,
            mesh: None,
            bounds: None,
        }
    }

    pub fn mesh(&self) -> Option<&Arc<Mesh>> {
        self.mesh.as_ref()
    }

    pub fn set_mesh(&mut self, dev: &wgpu::Device, mesh: Option<Arc<Mesh>>) {
        self.vbuf = None;
        self.ebuf = None;
        self.bounds = None;
        if let Some(m) = &mesh {
            // Per-triangle face ids need unshared vertices only where faces meet;
            // our mesh already has per-face vertices, so take face from the triangle.
            let mut vface = vec![u32::MAX; m.positions.len()];
            for (t, tri) in m.indices.chunks(3).enumerate() {
                for &i in tri {
                    vface[i as usize] = m.tri_face[t];
                }
            }
            let verts: Vec<Vertex> = (0..m.positions.len())
                .map(|i| Vertex { pos: m.positions[i], nor: m.normals[i], face: vface[i] })
                .collect();
            if !verts.is_empty() {
                let vb = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("rw-verts"),
                    contents: bytemuck::cast_slice(&verts),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                let ib = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("rw-idx"),
                    contents: bytemuck::cast_slice(&m.indices),
                    usage: wgpu::BufferUsages::INDEX,
                });
                self.vbuf = Some((vb, ib, m.indices.len() as u32));
            }
            let mut lines = vec![];
            for pl in &m.edges {
                for s in pl.windows(2) {
                    let c = [0.05, 0.05, 0.08, 1.0];
                    lines.push(LineVertex { pos: s[0], color: c });
                    lines.push(LineVertex { pos: s[1], color: c });
                }
            }
            if !lines.is_empty() {
                let eb = dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("rw-edges"),
                    contents: bytemuck::cast_slice(&lines),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                self.ebuf = Some((eb, lines.len() as u32));
            }
            let mp = m.mass_props();
            self.bounds = Some((Vec3::from(mp.bbox_min.map(|v| v as f32)), Vec3::from(mp.bbox_max.map(|v| v as f32))));
        }
        self.mesh = mesh;
    }

    /// Lines drawn on top of the model (sketches, origin axes).
    pub fn set_overlay(&mut self, dev: &wgpu::Device, lines: &[LineVertex]) {
        self.overlay = if lines.is_empty() {
            None
        } else {
            Some((
                dev.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("rw-overlay"),
                    contents: bytemuck::cast_slice(lines),
                    usage: wgpu::BufferUsages::VERTEX,
                }),
                lines.len() as u32,
            ))
        };
    }

    pub fn fit(&mut self, extra: Option<(Vec3, Vec3)>, animate: bool) {
        let b = match (self.bounds, extra) {
            (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.max(b.1))),
            (a, b) => a.or(b),
        };
        let (lo, hi) = b.unwrap_or((Vec3::splat(-50.0), Vec3::splat(50.0)));
        let target = (lo + hi) * 0.5;
        let dist = ((hi - lo).length() * 1.55).max(1.0);
        if animate {
            self.cam.animate_to(self.cam.rot, target, dist);
        } else {
            self.cam.target = target;
            self.cam.dist = dist;
        }
    }

    pub fn view_to(&mut self, eye: Vec3, up: Vec3) {
        let (lo, hi) = self.bounds.unwrap_or((Vec3::splat(-50.0), Vec3::splat(50.0)));
        self.cam.animate_to(look_rot(eye, up), (lo + hi) * 0.5, ((hi - lo).length() * 1.55).max(1.0));
    }

    /// Ray-cast the mesh; returns (face index, hit point).
    pub fn pick(&self, px: f32, py: f32, w: f32, h: f32) -> Option<(u32, Vec3)> {
        let m = self.mesh.as_ref()?;
        let (o, d) = self.cam.ray(px, py, w, h);
        let mut best: Option<(f32, u32)> = None;
        for (t, tri) in m.indices.chunks(3).enumerate() {
            let a = Vec3::from(m.positions[tri[0] as usize]);
            let b = Vec3::from(m.positions[tri[1] as usize]);
            let c = Vec3::from(m.positions[tri[2] as usize]);
            // Möller–Trumbore
            let e1 = b - a;
            let e2 = c - a;
            let p = d.cross(e2);
            let det = e1.dot(p);
            if det.abs() < 1e-12 {
                continue;
            }
            let inv = 1.0 / det;
            let s = o - a;
            let u = s.dot(p) * inv;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let q = s.cross(e1);
            let v = d.dot(q) * inv;
            if v < 0.0 || u + v > 1.0 {
                continue;
            }
            let dist = e2.dot(q) * inv;
            if dist > 0.0 && best.map_or(true, |(bd, _)| dist < bd) {
                best = Some((dist, m.tri_face[t]));
            }
        }
        best.map(|(dist, f)| (f, o + d * dist))
    }

    /// Renders into the offscreen target and returns the egui texture to show.
    pub fn render(&mut self, rs: &egui_wgpu::RenderState, size: [u32; 2]) -> Option<egui::TextureId> {
        let size = [size[0].max(1), size[1].max(1)];
        let dev = &rs.device;
        if self.targets.as_ref().map(|t| t.size) != Some(size) {
            let tex = |fmt, samples, usage| {
                dev.create_texture(&wgpu::TextureDescriptor {
                    label: None,
                    size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format: fmt,
                    usage,
                    view_formats: &[],
                })
            };
            let msaa = tex(COLOR_FMT, MSAA, wgpu::TextureUsages::RENDER_ATTACHMENT).create_view(&Default::default());
            let resolve = tex(COLOR_FMT, 1, wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING);
            let resolve_view = resolve.create_view(&Default::default());
            let depth = tex(DEPTH_FMT, MSAA, wgpu::TextureUsages::RENDER_ATTACHMENT).create_view(&Default::default());
            let mut r = rs.renderer.write();
            match self.tex_id {
                Some(id) => r.update_egui_texture_from_wgpu_texture(dev, &resolve_view, wgpu::FilterMode::Linear, id),
                None => self.tex_id = Some(r.register_native_texture(dev, &resolve_view, wgpu::FilterMode::Linear)),
            }
            self.targets = Some(Targets { size, msaa, resolve, resolve_view, depth });
        }
        let t = self.targets.as_ref().unwrap();
        let aspect = size[0] as f32 / size[1] as f32;
        let vp = self.cam.proj(aspect) * self.cam.view();
        let eye = self.cam.eye_dir();
        let light = (eye + self.cam.rot * Vec3::new(-0.35, 0.55, 0.0)).normalize();
        let u = Uniforms {
            view_proj: vp.to_cols_array_2d(),
            light: [light.x, light.y, light.z, 0.0],
            eye: [eye.x, eye.y, eye.z, 0.0],
            base: [0.62, 0.68, 0.76, 1.0],
            hl: [self.hovered.unwrap_or(u32::MAX), self.selected_face.unwrap_or(u32::MAX), 0, 0],
        };
        rs.queue.write_buffer(&self.ubuf, 0, bytemuck::bytes_of(&u));
        let mut enc = dev.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rw-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.msaa,
                    resolve_target: Some(&t.resolve_view),
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.bind, &[]);
            if let Some((vb, ib, n)) = &self.vbuf {
                pass.set_pipeline(&self.mesh_pipe);
                pass.set_vertex_buffer(0, vb.slice(..));
                pass.set_index_buffer(ib.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..*n, 0, 0..1);
            }
            pass.set_pipeline(&self.line_pipe);
            if self.show_edges {
                if let Some((eb, n)) = &self.ebuf {
                    pass.set_vertex_buffer(0, eb.slice(..));
                    pass.draw(0..*n, 0..1);
                }
            }
            if let Some((ob, n)) = &self.overlay {
                pass.set_vertex_buffer(0, ob.slice(..));
                pass.draw(0..*n, 0..1);
            }
        }
        rs.queue.submit([enc.finish()]);
        let _ = &t.resolve;
        self.tex_id
    }
}
