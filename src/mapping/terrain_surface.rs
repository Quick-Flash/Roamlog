//! Event-driven GPU worker, adapted from Enlumi's surface producer.
use super::{
    camera::Camera,
    terrain::{Orbit, Scene},
    terrain_gpu,
};
use std::sync::{Arc, Condvar, Mutex};
use zgui::surface::{GpuShare, SurfaceConfig, SurfaceEvent, SurfaceHandle, wgpu};

#[derive(Clone)]
struct State {
    gpu: Option<GpuShare>,
    size: [u32; 2],
    scale: f32,
    camera: Camera,
    orbit: Orbit,
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
    _endpoint: Arc<SurfaceHandle>,
    shared: Arc<Shared>,
}
impl Producer {
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                gpu: None,
                size: [0, 0],
                scale: 1.,
                camera: Camera::default(),
                orbit: Orbit::default(),
                scene: Arc::new(Scene::default()),
                visible: true,
                closed: false,
                revision: 0,
            }),
            changed: Condvar::new(),
        });
        let handle = SurfaceHandle::new(SurfaceConfig::default());
        let endpoint = Arc::new(handle.clone());
        let output = Arc::downgrade(&endpoint);
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
                SurfaceEvent::DeviceLost { gpu } => {
                    if let Some(output) = output.upgrade() {
                        output.present(recovery_image(&gpu));
                    }
                    s.gpu = Some(gpu);
                }
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
        Self {
            handle,
            shared,
            _endpoint: endpoint,
        }
    }
    pub fn update(&self, camera: Camera, orbit: Orbit, scene: Arc<Scene>) {
        let mut s = self.shared.state.lock().unwrap();
        if s.camera == camera && s.orbit == orbit && Arc::ptr_eq(&s.scene, &scene) {
            return;
        }
        s.camera = camera;
        s.orbit = orbit;
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
    let mut renderer: Option<(u64, terrain_gpu::Renderer)> = None;
    let mut in_flight: Option<(u64, wgpu::SubmissionIndex)> = None;
    let mut failed = None;
    loop {
        let state = {
            let mut s = shared.state.lock().unwrap();
            while !s.closed
                && (s.gpu.is_none()
                    || !s.visible
                    || s.size.contains(&0)
                    || revision == Some(s.revision)
                    || s.gpu
                        .as_ref()
                        .is_some_and(|g| Some(g.generation()) == failed))
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
            renderer = Some((
                gpu.generation(),
                terrain_gpu::Renderer::new(device, gpu.queue()),
            ));
        }
        // Bound output memory on high-DPI phones. Never block Android UI on GPU work.
        let limit = f64::from(device.limits().max_texture_dimension_2d);
        let shrink = (limit / f64::from(state.size[0].max(state.size[1])))
            .min((4_194_304. / (f64::from(state.size[0]) * f64::from(state.size[1]))).sqrt())
            .min(1.);
        let extent = state
            .size
            .map(|v| (f64::from(v) * shrink).floor().max(1.) as u32);
        let texture = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("roamlog.terrain"),
            size: wgpu::Extent3d {
                width: extent[0],
                height: extent[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        }));
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.as_mut().unwrap().1.render(
            gpu,
            &mut encoder,
            terrain_gpu::Frame {
                output: &texture.create_view(&Default::default()),
                extent,
                camera: state.camera,
                orbit: state.orbit,
                scene: &state.scene,
                css: state.size.map(|v| f64::from(v) / f64::from(state.scale)),
            },
        );
        // At most one frame in flight; CPU prepares next while previous GPU frame runs.
        if let Some((generation, submission)) = in_flight.take()
            && generation == gpu.generation()
            && let Err(error) = device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(std::time::Duration::from_secs(1)),
            })
        {
            eprintln!("Terrain GPU submission failed: {error}");
            failed = Some(gpu.generation());
            renderer = None;
            continue;
        }
        in_flight = Some((gpu.generation(), gpu.queue().submit([encoder.finish()])));
        let current = shared.state.lock().unwrap();
        if !current.closed
            && current.visible
            && current.size == state.size
            && current.scale == state.scale
            && current
                .gpu
                .as_ref()
                .is_some_and(|g| g.generation() == gpu.generation())
        {
            output.present(texture);
        }
    }
}

fn recovery_image(gpu: &GpuShare) -> Arc<wgpu::Texture> {
    let size = wgpu::Extent3d {
        width: 1,
        height: 1,
        depth_or_array_layers: 1,
    };
    let texture = Arc::new(gpu.device().create_texture(&wgpu::TextureDescriptor {
        label: Some("map.recovery"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    }));
    gpu.queue().write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[18, 24, 30, 255],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        size,
    );
    texture
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unchanged_camera_reuses_scene_without_queuing_another_gpu_frame() {
        let producer = Producer::new();
        let scene = Arc::new(Scene::default());
        let camera = Camera::default();
        let orbit = Orbit::default();
        producer.update(camera, orbit, scene.clone());
        let revision = producer.shared.state.lock().unwrap().revision;
        producer.update(camera, orbit, scene.clone());
        assert_eq!(producer.shared.state.lock().unwrap().revision, revision);
        producer.update(
            Camera {
                zoom: camera.zoom + 0.1,
                ..camera
            },
            orbit,
            scene,
        );
        assert_eq!(producer.shared.state.lock().unwrap().revision, revision + 1);
    }
}
