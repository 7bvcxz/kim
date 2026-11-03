//! GUI evaluation baseline: a bare winit window cleared by softbuffer (CPU), no text, no UI
//! toolkit. It is the lower bound for "a native window exists" costs.
//!
//! Usage: eval-winit <seconds> [--onscreen]

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

struct App {
    start: Instant,
    deadline: Instant,
    onscreen: bool,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    reported: bool,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        let mut attrs = Window::default_attributes()
            .with_title("kim gui-eval (winit)")
            .with_inner_size(winit::dpi::LogicalSize::new(960.0, 640.0));
        if !self.onscreen {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs
                .with_position(winit::dpi::PhysicalPosition::new(-6000, -6000))
                .with_active(false)
                .with_skip_taskbar(true);
        }
        let window = Rc::new(el.create_window(attrs).expect("window"));
        let context = softbuffer::Context::new(window.clone()).expect("context");
        self.surface = Some(softbuffer::Surface::new(&context, window.clone()).expect("surface"));
        self.window = Some(window);
        el.set_control_flow(ControlFlow::WaitUntil(self.deadline));
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => {
                let (Some(window), Some(surface)) = (&self.window, &mut self.surface) else {
                    return;
                };
                let size = window.inner_size();
                let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                else {
                    return;
                };
                surface.resize(w, h).expect("resize");
                let mut buf = surface.buffer_mut().expect("buffer");
                buf.fill(0x000e_0e12);
                buf.present().expect("present");
                if !self.reported {
                    self.reported = true;
                    println!("first_frame_ms={}", self.start.elapsed().as_millis());
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if Instant::now() >= self.deadline {
            el.exit();
        } else {
            el.set_control_flow(ControlFlow::WaitUntil(self.deadline));
        }
    }
}

fn main() {
    let start = Instant::now();
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let onscreen = std::env::args().any(|a| a == "--onscreen");
    let el = EventLoop::new().expect("event loop");
    let mut app = App {
        start,
        deadline: start + Duration::from_secs(secs),
        onscreen,
        window: None,
        surface: None,
        reported: false,
    };
    el.run_app(&mut app).expect("run");
}
