use std::cmp::max;
use std::collections::HashMap;
use std::error::Error;
use std::{fs::File, os::unix::prelude::AsFd};

use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{
    delegate_noop,
    protocol::{
        wl_buffer, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
        wl_touch,
    },
    Connection, Dispatch, Proxy, QueueHandle, WEnum,
};

use wayland_protocols::xdg::shell::client::xdg_surface::XdgSurface;
use wayland_protocols::xdg::shell::client::xdg_wm_base::XdgWmBase;
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

fn main() -> Result<(), Box<dyn Error>> {
    let conn = Connection::connect_to_env()?;

    let mut event_queue = conn.new_event_queue();
    let qhandle = event_queue.handle();

    let display = conn.display();
    display.get_registry(&qhandle, ());

    let mut state = State {
        running: true,
        compositor: None,
        wm_base: None,
        wl_shm: None,
        initialised: None,
        pointer_x: 0f64,
        pointer_y: 0f64,
        pointer_width: 0f64,
        pointer_height: 0f64,
    };

    println!("Starting the calibrate-touchscreen app: touch the target spots.");
    println!("(Or press <ESC> to quit!)");

    while state.not_done() {
        event_queue.blocking_dispatch(&mut state)?;
    }

    Ok(())
}

struct FullscreenSurface {
    width: i32,
    height: i32,
    wl_surface: WlSurface,
}

impl FullscreenSurface {
    fn attach_buffer(&self, wl_shm: &WlShm, qh: &QueueHandle<State>, targets_index: usize) {
        let width = self.width;
        let height = self.height;

        let mut file = tempfile::tempfile().unwrap();
        draw_with_target(&mut file, (width, height), TARGETS[targets_index]);
        let pool = wl_shm.create_pool(file.as_fd(), width * height * 4, qh, ());
        let buffer = pool.create_buffer(
            0,
            width,
            height,
            width * 4,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );

        self.wl_surface.attach(Some(&buffer), 0, 0);
        self.wl_surface.commit();
        buffer.destroy();
    }
}

// We need targets at three points (not on a line)
static TARGETS: [(f64, f64); 3] = [(0.2f64, 0.4f64), (0.8f64, 0.6f64), (0.4f64, 0.8f64)];

struct State {
    running: bool,
    initialised: Option<InitialisedState>,
    compositor: Option<WlCompositor>,
    wm_base: Option<XdgWmBase>,
    wl_shm: Option<WlShm>,

    // We shouldn't calibrate using the mouse, but this makes testing easier
    pointer_x: f64,
    pointer_y: f64,
    pointer_width: f64,
    pointer_height: f64,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version: _,
            } => match &interface[..] {
                "wl_output" => {
                    registry.bind::<WlOutput, _, _>(name, 1, qh, ());
                }
                "wl_compositor" => {
                    let compositor = registry.bind::<WlCompositor, _, _>(name, 1, qh, ());
                    state.compositor = Some(compositor);
                }
                "wl_shm" => {
                    let shm = registry.bind::<WlShm, _, _>(name, 1, qh, ());
                    state.wl_shm = Some(shm);
                }
                "wl_seat" => {
                    registry.bind::<wl_seat::WlSeat, _, _>(name, 1, qh, ());
                }
                "xdg_wm_base" => {
                    let wm_base = registry.bind::<XdgWmBase, _, _>(name, 1, qh, ());
                    state.wm_base = Some(wm_base);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_output::Mode;

        match event {
            wl_output::Event::Mode {
                flags: WEnum::Value(flags),
                width,
                height,
                refresh: _,
            } => {
                if flags.contains(Mode::Current) {
                    if state.initialised.is_none() {
                        let Some(compositor) = state.compositor.take() else {
                            unreachable!("Missing compositor global");
                        };
                        let Some(wm_base) = state.wm_base.take() else {
                            unreachable!("Missing wm_base global");
                        };
                        let Some(wl_shm) = state.wl_shm.take() else {
                            unreachable!("Missing wl_shm global");
                        };

                        state.initialised = Some(InitialisedState {
                            compositor: compositor,
                            wm_base: wm_base,
                            wl_shm: wl_shm,
                            outputs: HashMap::new(),
                            targets_index: 0,
                            touches: [(0f64, 0f64), (0f64, 0f64), (0f64, 0f64)],
                        });
                    }

                    state.init_output(output, qh, width, height);
                }
            }

            _ => {}
        }
    }
}

fn draw_with_target(tmp: &mut File, (buf_x, buf_y): (i32, i32), (target_x, target_y): (f64, f64)) {
    let centre_x = (buf_x as f64 * target_x) as i64;
    let centre_y = (buf_y as f64 * target_y) as i64;
    let target_size = (max(buf_x, buf_y) / 80) as i64;
    use std::io::Write;
    let mut buf = std::io::BufWriter::new(tmp);
    for y in 0..buf_y as i64 {
        for x in 0..buf_x as i64 {
            let distance_squared =
                (x - centre_x) * (x - centre_x) + (y - centre_y) * (y - centre_y);
            if distance_squared >= target_size * target_size {
                let a = 0xFF;
                let r = 0x3F;
                let g = 0x3F * y as i32 / buf_y;
                let b = 0x3F * y as i32 / buf_y;
                buf.write_all(&[b as u8, g as u8, r as u8, a as u8])
                    .unwrap();
            } else {
                let intensity = 0xFF - 0x40 * (distance_squared / (target_size * target_size / 4));
                let a = 0xFF;
                buf.write_all(&[intensity as u8, intensity as u8, intensity as u8, a as u8])
                    .unwrap();
            }
        }
    }
    buf.flush().unwrap();
}

impl State {
    fn process_touch(&mut self, touch_x: f64, touch_y: f64, qh: &QueueHandle<State>) {
        self.initialised
            .as_mut()
            .unwrap()
            .process_touch(touch_x, touch_y, qh);
    }

    fn size_of(&mut self, surface: &WlSurface) -> (f64, f64) {
        self.initialised.as_mut().unwrap().size_of(&surface)
    }

    fn not_done(&mut self) -> bool {
        self.running
            && (self.initialised.is_none() || !self.initialised.as_ref().unwrap().is_done())
    }

    fn init_output(&mut self, output: &WlOutput, qh: &QueueHandle<State>, width: i32, height: i32) {
        self.initialised
            .as_mut()
            .unwrap()
            .init_output(output, width, height, qh);
    }
}

struct InitialisedState {
    compositor: WlCompositor,
    wm_base: XdgWmBase,
    wl_shm: WlShm,
    outputs: HashMap<WlOutput, FullscreenSurface>,

    targets_index: usize,
    touches: [(f64, f64); 3],
}

impl InitialisedState {
    fn init_output(&mut self, output: &WlOutput, width: i32, height: i32, qh: &QueueHandle<State>) {
        let surface = FullscreenSurface {
            width: width,
            height: height,
            wl_surface: init_surface(qh, &self.compositor, &self.wm_base, &output),
        };

        self.outputs.insert(output.clone(), surface);
    }

    fn draw_fullscreen_surfaces(&mut self, qh: &QueueHandle<State>) {
        for (_, window) in self.outputs.iter_mut() {
            let targets_index = self.targets_index;
            window.attach_buffer(&self.wl_shm, qh, targets_index);
        }
    }

    fn process_touch(&mut self, touch_x: f64, touch_y: f64, qh: &QueueHandle<State>) {
        self.touches[self.targets_index] = (touch_x, touch_y);
        self.targets_index += 1;
        if self.targets_index != TARGETS.len() {
            self.draw_fullscreen_surfaces(qh);
        } else {
            // This is a well known solution to this linear algebra problem
            let k = (self.touches[0].0 - self.touches[2].0)
                * (self.touches[1].1 - self.touches[2].1)
                - (self.touches[1].0 - self.touches[2].0) * (self.touches[0].1 - self.touches[2].1);

            let ak = (TARGETS[0].0 - TARGETS[2].0) * (self.touches[1].1 - self.touches[2].1)
                - (TARGETS[1].0 - TARGETS[2].0) * (self.touches[0].1 - self.touches[2].1);

            let bk = (self.touches[0].0 - self.touches[2].0) * (TARGETS[1].0 - TARGETS[2].0)
                - (TARGETS[0].0 - TARGETS[2].0) * (self.touches[1].0 - self.touches[2].0);

            let ck = self.touches[0].1
                * (self.touches[2].0 * TARGETS[1].0 - self.touches[1].0 * TARGETS[2].0)
                + self.touches[1].1
                    * (self.touches[0].0 * TARGETS[2].0 - self.touches[2].0 * TARGETS[0].0)
                + self.touches[2].1
                    * (self.touches[1].0 * TARGETS[0].0 - self.touches[0].0 * TARGETS[1].0);

            let dk = (TARGETS[0].1 - TARGETS[2].1) * (self.touches[1].1 - self.touches[2].1)
                - (TARGETS[1].1 - TARGETS[2].1) * (self.touches[0].1 - self.touches[2].1);

            let ek = (self.touches[0].0 - self.touches[2].0) * (TARGETS[1].1 - TARGETS[2].1)
                - (TARGETS[0].1 - TARGETS[2].1) * (self.touches[1].0 - self.touches[2].0);

            let fk = self.touches[0].1
                * (self.touches[2].0 * TARGETS[1].1 - self.touches[1].0 * TARGETS[2].1)
                + self.touches[1].1
                    * (self.touches[0].0 * TARGETS[2].1 - self.touches[2].0 * TARGETS[0].1)
                + self.touches[2].1
                    * (self.touches[1].0 * TARGETS[0].1 - self.touches[0].0 * TARGETS[1].1);

            println!(
                "Calibration = {:.3} {:.3} {:.3} {:.3} {:.3} {:.3}",
                ak / k,
                bk / k,
                ck / k,
                dk / k,
                ek / k,
                fk / k
            );
        }
    }

    fn size_of(&self, surface: &WlSurface) -> (f64, f64) {
        for (_, fs) in &self.outputs {
            if &fs.wl_surface == surface {
                return (fs.width as f64, fs.height as f64);
            };
        }
        unreachable!("The loop should always return");
    }

    fn is_done(&self) -> bool {
        self.targets_index == TARGETS.len()
    }
}

fn init_surface(
    qh: &QueueHandle<State>,
    compositor: &WlCompositor,
    wm_base: &XdgWmBase,
    output: &WlOutput,
) -> WlSurface {
    let surface = compositor.create_surface(qh, ());

    let xdg_surface = wm_base.get_xdg_surface(&surface, qh, ());
    let toplevel = xdg_surface.get_toplevel(qh, ());

    toplevel.set_title(output.id().to_string());
    toplevel.set_fullscreen(Some(output));
    surface.commit();
    surface
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close {} = event {
            state.running = false;
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_seat::Event::Capabilities {
                capabilities: WEnum::Value(capabilities),
            } => {
                if capabilities.contains(wl_seat::Capability::Keyboard) {
                    seat.get_keyboard(qh, ());
                }
                if capabilities.contains(wl_seat::Capability::Pointer) {
                    seat.get_pointer(qh, ());
                }
                if capabilities.contains(wl_seat::Capability::Touch) {
                    seat.get_touch(qh, ());
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Key { key: 1, .. } => {
                // ESC key
                state.running = false;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_touch::WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down {
                serial: _,
                time: _,
                surface,
                id: _,
                x,
                y,
            } => {
                let (width, height): (f64, f64) = state.size_of(&surface);
                state.process_touch(x / width, y / height, qh);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Button {
                serial: _,
                time: _,
                button: _,
                state: WEnum::Value(bstate),
            } => {
                if bstate == wl_pointer::ButtonState::Pressed {
                    state.process_touch(
                        state.pointer_x / state.pointer_width,
                        state.pointer_y / state.pointer_height,
                        qh,
                    );
                }
            }
            wl_pointer::Event::Enter {
                serial: _,
                surface,
                surface_x,
                surface_y,
            } => {
                state.pointer_x = surface_x;
                state.pointer_y = surface_y;

                (state.pointer_width, state.pointer_height) = state.size_of(&surface);
            }
            wl_pointer::Event::Motion {
                time: _,
                surface_x,
                surface_y,
            } => {
                state.pointer_x = surface_x;
                state.pointer_y = surface_y;
            }
            _ => {}
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        proxy: &XdgSurface,
        event: xdg_surface::Event,
        _data: &(),
        _conn: &Connection,
        qhandle: &QueueHandle<Self>,
    ) {
        match event {
            xdg_surface::Event::Configure { serial } => {
                proxy.ack_configure(serial);
                state
                    .initialised
                    .as_mut()
                    .unwrap()
                    .draw_fullscreen_surfaces(qhandle);
            }
            _ => {}
        }
    }
}

// Ignore events from these object types in this example.
delegate_noop!(State: ignore WlCompositor);
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
