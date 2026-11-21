//! Surface worker for GPU map: redraws only when camera or scene changes.
use super::{
    camera::Camera,
    map_gpu::{Frame, Renderer, Scene},
};
use std::sync::{Arc, Condvar, Mutex};
use zgui::surface::{GpuShare, SurfaceConfig, SurfaceEvent, SurfaceHandle, wgpu};

#[derive(Clone)]
struct State {
    gpu: Option<GpuShare>,
    size: [u32; 2],
    scale: f32,
    camera: Camera,
    scene: Arc<Scene>,
    visible: bool,
    closed: bool,
    revision: u64,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}
pub struct Producer {
    pub handle: SurfaceHandle,
    shared: Arc<Shared>,
}

impl Producer {
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                gpu: None,
                size: [0, 0],
                scale: 1.0,
                camera: Camera::default(),
                scene: Arc::default(),
                visible: true,
                closed: false,
                revision: 0,
            }),
            changed: Condvar::new(),
        });
        let handle = SurfaceHandle::new(SurfaceConfig::default());
        let events = shared.clone();
        handle.set_events(move |event| {
            let mut s = events.state.lock().unwrap();
            match event {
                SurfaceEvent::Attached { gpu, size, scale } => {
                    s.gpu = Some(gpu);
                    s.size = [size.width, size.height];
                    s.scale = scale;
                    s.visible = true;
                }
                SurfaceEvent::Resized { size, scale } => {
                    s.size = [size.width, size.height];
                    s.scale = scale;
                }
                SurfaceEvent::Visible(visible) => s.visible = visible,
                SurfaceEvent::DeviceLost { gpu } => s.gpu = Some(gpu),
                SurfaceEvent::Detached => {
                    s.gpu = None;
                    s.visible = false;
                }
            }
            s.revision = s.revision.wrapping_add(1);
            events.changed.notify_one();
        });
        let worker = shared.clone();
        let output = handle.clone();
        std::thread::spawn(move || produce(worker, output));
        Self { handle, shared }
    }

    pub fn update(&self, camera: Camera, scene: Arc<Scene>) {
        let mut s = self.shared.state.lock().unwrap();
        if s.camera == camera && Arc::ptr_eq(&s.scene, &scene) {
            return;
        }
        s.camera = camera;
        s.scene = scene;
        s.revision = s.revision.wrapping_add(1);
        self.shared.changed.notify_one();
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        let mut s = self.shared.state.lock().unwrap();
        s.closed = true;
        self.shared.changed.notify_one();
    }
}

fn produce(shared: Arc<Shared>, output: SurfaceHandle) {
    let mut revision = None;
    let mut renderer: Option<(u64, Renderer)> = None;
    // Textures reused once compositor releases them (only pool holds a reference).
    let mut pool: Vec<Arc<wgpu::Texture>> = Vec::new();
    let mut in_flight: Option<(u64, wgpu::SubmissionIndex)> = None;
    loop {
        let state = {
            let mut s = shared.state.lock().unwrap();
            while !s.closed
                && (s.gpu.is_none()
                    || !s.visible
                    || s.size.contains(&0)
                    || revision == Some(s.revision))
            {
                s = shared.changed.wait(s).unwrap();
            }
            if s.closed {
                return;
            }
            s.clone()
        };
        revision = Some(state.revision);
        let gpu = state.gpu.as_ref().unwrap();
        let device = gpu.device();
        if renderer
            .as_ref()
            .is_none_or(|(generation, _)| *generation != gpu.generation())
        {
            renderer = Some((gpu.generation(), Renderer::new(device)));
            pool.clear();
        }
        // Rotated map is screen diagonal square; cap output at ~6 MP.
        let limit = f64::from(device.limits().max_texture_dimension_2d);
        let shrink = (limit / f64::from(state.size[0].max(state.size[1])))
            .min((6.0e6 / (f64::from(state.size[0]) * f64::from(state.size[1]))).sqrt())
            .min(1.0);
        let extent = state
            .size
            .map(|v| (f64::from(v) * shrink).floor().max(1.0) as u32);
        pool.retain(|t| t.width() == extent[0] && t.height() == extent[1]);
        let texture = match pool.iter().find(|t| Arc::strong_count(t) == 1) {
            Some(t) => t.clone(),
            None => {
                let t = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("map.output"),
                    size: wgpu::Extent3d {
                        width: extent[0],
                        height: extent[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                }));
                if pool.len() < 3 {
                    pool.push(t.clone());
                }
                t
            }
        };
        let css = state.size.map(|v| f64::from(v) / f64::from(state.scale));
        let ratio = (f64::from(extent[0]) / css[0]) as f32;
        let mut encoder = device.create_command_encoder(&Default::default());
        let waiting = renderer.as_mut().unwrap().1.render(
            device,
            gpu.queue(),
            &mut encoder,
            &state.scene,
            Frame {
                output: &texture.create_view(&Default::default()),
                extent,
                camera: state.camera,
                css,
                ratio,
            },
        );
        // At most one frame in flight.
        if let Some((generation, submission)) = in_flight.take()
            && generation == gpu.generation()
        {
            let _ = device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(std::time::Duration::from_secs(1)),
            });
        }
        in_flight = Some((gpu.generation(), gpu.queue().submit([encoder.finish()])));
        let current = shared.state.lock().unwrap();
        if !current.closed
            && current.visible
            && current.size == state.size
            && current
                .gpu
                .as_ref()
                .is_some_and(|g| g.generation() == gpu.generation())
        {
            output.present(texture);
        }
        // Imagery uploads spread over frames: render again without waiting for a change.
        if waiting {
            revision = None;
        }
    }
}
