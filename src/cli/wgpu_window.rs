use anyhow::anyhow;
use anyhow::Result;
use bevy_ecs::world::World;
use std::num::NonZeroU32;
use std::time::Duration;
use std::time::Instant;
use winit::event::Event;
use winit::event::WindowEvent;
use winit::event_loop::ControlFlow;
use winit::event_loop::EventLoop;
use winit::window::Window;
use winit::window::WindowBuilder;

use crate::core::app_controller::Theme;

const INITIAL_WINDOW_WIDTH: u32 = 800;
const INITIAL_WINDOW_HEIGHT: u32 = 600;

struct WgpuState {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: winit::dpi::PhysicalSize<u32>,
    clear_color: wgpu::Color,
}

impl WgpuState {
    async fn new(window: &Window, theme: Theme) -> Result<Self> {
        let size = window.inner_size();

        let clear_color = match theme {
            Theme::Light => wgpu::Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            Theme::Dark => wgpu::Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        };

        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        // `wgpu::Surface` 的生命周期受 window 约束。这里用 `transmute` 把生命周期延长到 'static，
        // 前提是我们保证 window 在 event loop 生命周期内不被 drop（确实如此：window 被 move 进闭包）。
        let surface = instance
            .create_surface(window)
            .map_err(|e| anyhow!("Failed to create wgpu surface: {e}"))?;
        let surface: wgpu::Surface<'static> = unsafe { std::mem::transmute(surface) };

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| anyhow!("No suitable GPU adapters found"))?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("layout-viewer device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                },
                None,
            )
            .await
            .map_err(|e| anyhow!("Failed to request device: {e}"))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            caps.present_modes[0]
        };

        let alpha_mode = caps.alpha_modes[0];

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        surface.configure(&device, &config);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            size,
            clear_color,
        })
    }

    fn resize(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.size = new_size;
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);
    }

    fn render(&mut self) -> Result<()> {
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            Err(wgpu::SurfaceError::OutOfMemory) => {
                return Err(anyhow!("wgpu: out of memory"));
            }
            Err(e) => {
                // Timeout / Outdated 等：下一帧重试即可
                log::warn!("wgpu surface error: {e:?}");
                return Ok(());
            }
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("layout-viewer wgpu encoder"),
            });

        {
            let _rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("layout-viewer clear pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set: None,
                timestamp_writes: None,
            });
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}

pub fn spawn_wgpu_window(world: World, theme: Theme) -> Result<()> {
    // 先保留 world（未来会在 wgpu backend 中消费），避免 unused 警告。
    let _world = world;

    let event_loop = EventLoop::new()?;
    let window = WindowBuilder::new()
        .with_title("Layout Viewer (wgpu)")
        .with_inner_size(winit::dpi::LogicalSize::new(
            INITIAL_WINDOW_WIDTH,
            INITIAL_WINDOW_HEIGHT,
        ))
        .build(&event_loop)?;

    let mut state = pollster::block_on(WgpuState::new(&window, theme))?;

    let mut next_tick = Instant::now();
    let tick_interval = Duration::from_millis(16);

    let _ = event_loop.run(move |event, window_target| {
        if let Some(next_tick_time) = next_tick.checked_add(tick_interval) {
            window_target.set_control_flow(ControlFlow::WaitUntil(next_tick_time));
        }

        match event {
            Event::AboutToWait => {
                let now = Instant::now();
                if now >= next_tick {
                    window.request_redraw();
                    next_tick = now + tick_interval;
                }
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    window_target.exit();
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    use winit::keyboard::KeyCode;
                    use winit::keyboard::PhysicalKey;
                    if let PhysicalKey::Code(code) = event.physical_key {
                        if code == KeyCode::Escape || code == KeyCode::KeyQ {
                            window_target.exit();
                        }
                    }
                }
                WindowEvent::Resized(size) => {
                    // wgpu 要求宽高非 0；winit 最小化时会给 0
                    let width = NonZeroU32::new(size.width);
                    let height = NonZeroU32::new(size.height);
                    if width.is_some() && height.is_some() {
                        state.resize(size);
                    }
                }
                WindowEvent::RedrawRequested => {
                    if let Err(err) = state.render() {
                        log::error!("wgpu render failed: {err:#}");
                        window_target.exit();
                    }
                }
                _ => (),
            },
            _ => (),
        }
    });

    Ok(())
}
