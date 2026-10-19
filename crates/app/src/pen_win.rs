//! Win32 pen reading: subclasses the eframe window procedure and turns WinTab packets
//! or Windows Ink `WM_POINTER` messages into `PenEvent`s. Ported from `spikes/pen-test`.

use crate::input::{
    PenApi, PenEvent, PhaseTracker, active_api, normalise_pressure, tilt_from_orientation,
    wintab_is_eraser,
};
use std::{cell::RefCell, ffi::c_void, mem::zeroed, ptr::null_mut};
use windows_sys::Win32::{
    Foundation::*, Graphics::Gdi::ScreenToClient, System::LibraryLoader::*, UI::Input::Pointer::*,
    UI::Shell::*, UI::WindowsAndMessaging::*,
};
use windows_sys::core::BOOL;

const PEN_FLAG_BARREL_: u32 = 0x1;
const PEN_FLAG_INVERTED_: u32 = 0x2;
const PEN_FLAG_ERASER_: u32 = 0x4;
const INK_MAX: u32 = 1024;

// wintab.h. Packet field order follows the bit order of PK_DATA.
const WTI_DEFSYSCTX: u32 = 4;
const WTI_DEVICES: u32 = 100;
const DVC_NAME: u32 = 1;
const DVC_NPRESSURE: u32 = 15;
const CXO_MESSAGES: u32 = 0x0004;
const PK_DATA: u32 = 0x0002 | 0x0004 | 0x0020 | 0x0040 | 0x0080 | 0x0100 | 0x0400 | 0x1000;

#[repr(C)]
#[derive(Clone, Copy)]
struct LogContext {
    name: [u16; 40],
    options: u32,
    status: u32,
    locks: u32,
    msg_base: u32,
    device: u32,
    pkt_rate: u32,
    pkt_data: u32,
    pkt_mode: u32,
    move_mask: u32,
    btn_dn_mask: u32,
    btn_up_mask: u32,
    in_org: [i32; 3],
    in_ext: [i32; 3],
    out_org: [i32; 3],
    out_ext: [i32; 3],
    sens: [u32; 3],
    sys_mode: i32,
    sys_org_x: i32,
    sys_org_y: i32,
    sys_ext_x: i32,
    sys_ext_y: i32,
    sys_sens: [u32; 2],
}

#[repr(C)]
#[derive(Default)]
struct Axis {
    min: i32,
    max: i32,
    units: u32,
    resolution: u32,
}

#[repr(C)]
#[derive(Default)]
struct Packet {
    status: u32,
    time: u32,
    cursor: u32,
    buttons: u32,
    x: i32,
    y: i32,
    pressure: u32,
    azimuth: i32,
    altitude: i32,
    twist: i32,
}

type WtInfo = unsafe extern "system" fn(u32, u32, *mut c_void) -> u32;
type WtOpen = unsafe extern "system" fn(HWND, *mut LogContext, BOOL) -> *mut c_void;
type WtPacket = unsafe extern "system" fn(*mut c_void, u32, *mut Packet) -> BOOL;
type WtCtx = unsafe extern "system" fn(*mut c_void, BOOL) -> BOOL;

struct Wintab {
    packet: WtPacket,
    overlap: WtCtx,
    ctx: *mut c_void,
    msg: u32,
    pressure: (u32, u32),
}

struct State {
    api: PenApi,
    wintab: Option<Wintab>,
    phase: PhaseTracker,
    queue: Vec<PenEvent>,
    repaint: eframe::egui::Context,
}

thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

/// Which API ended up active, and the WinTab device name or why WinTab is not used.
pub struct Status {
    pub api: PenApi,
    pub wintab: String,
}

/// Starts pen reading on `hwnd`. Call once, on the window's thread.
pub fn install(hwnd: isize, setting: PenApi, repaint: eframe::egui::Context) -> Status {
    let hwnd = hwnd as HWND;
    let (wintab, note) = if setting == PenApi::WinTab {
        match unsafe { load_wintab(hwnd) } {
            Ok((w, device)) => (Some(w), device),
            Err(e) => (None, e),
        }
    } else {
        (None, "not selected".into())
    };
    let api = active_api(setting, wintab.is_some());
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            api,
            wintab,
            phase: PhaseTracker::default(),
            queue: Vec::new(),
            repaint,
        })
    });
    unsafe { SetWindowSubclass(hwnd, Some(subclass), 1, 0) };
    Status { api, wintab: note }
}

/// Takes the pen events that arrived since the last call.
pub fn drain() -> Vec<PenEvent> {
    STATE.with(|s| {
        s.borrow_mut()
            .as_mut()
            .map(|s| std::mem::take(&mut s.queue))
            .unwrap_or_default()
    })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

unsafe fn load_wintab(hwnd: HWND) -> Result<(Wintab, String), String> {
    unsafe {
        let lib = LoadLibraryW(wide("Wintab32.dll").as_ptr());
        if lib.is_null() {
            return Err("Wintab32.dll not found".into());
        }
        let f = |n: &[u8]| {
            GetProcAddress(lib, n.as_ptr()).ok_or(String::from_utf8_lossy(n).into_owned())
        };
        let info: WtInfo = std::mem::transmute(f(b"WTInfoW\0")?);
        let open: WtOpen = std::mem::transmute(f(b"WTOpenW\0")?);
        let packet: WtPacket = std::mem::transmute(f(b"WTPacket\0")?);
        let overlap: WtCtx = std::mem::transmute(f(b"WTOverlap\0")?);
        if info(0, 0, null_mut()) == 0 {
            return Err("WinTab service not available".into());
        }

        let mut name = [0u16; 128];
        info(WTI_DEVICES, DVC_NAME, name.as_mut_ptr().cast());
        let len = name.iter().position(|&c| c == 0).unwrap_or(0);
        let device = String::from_utf16_lossy(&name[..len]);
        let mut axis = Axis::default();
        info(WTI_DEVICES, DVC_NPRESSURE, (&mut axis as *mut Axis).cast());

        let mut lc: LogContext = zeroed();
        if info(WTI_DEFSYSCTX, 0, (&mut lc as *mut LogContext).cast()) == 0 {
            return Err("no default system context".into());
        }
        lc.options |= CXO_MESSAGES;
        lc.pkt_data = PK_DATA;
        lc.pkt_mode = 0;
        lc.move_mask = PK_DATA;
        // Output in screen pixels; the negative Y extent flips WinTab's Y-up origin.
        lc.out_org = [lc.sys_org_x, lc.sys_org_y, 0];
        lc.out_ext = [lc.sys_ext_x, -lc.sys_ext_y, lc.out_ext[2]];
        let ctx = open(hwnd, &mut lc, 1);
        if ctx.is_null() {
            return Err("WTOpenW failed".into());
        }
        let pressure = (axis.min.max(0) as u32, axis.max.max(1) as u32);
        let device = format!("{device} (pressure {}..{})", pressure.0, pressure.1);
        Ok((
            Wintab {
                packet,
                overlap,
                ctx,
                msg: lc.msg_base,
                pressure,
            },
            device,
        ))
    }
}

fn push(
    s: &mut State,
    hwnd: HWND,
    screen: POINT,
    pressure: f32,
    eraser: bool,
    tilt: [f32; 2],
    barrel: bool,
) {
    let mut pt = screen;
    unsafe { ScreenToClient(hwnd, &mut pt) };
    let phase = s.phase.next(pressure > 0.0);
    s.queue.push(PenEvent {
        pos: [pt.x as f32, pt.y as f32],
        pressure,
        eraser,
        tilt,
        barrel,
        phase,
    });
    s.repaint.request_repaint();
}

/// Returns true when the message is consumed.
unsafe fn handle(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> bool {
    STATE.with(|cell| {
        let mut guard = cell.borrow_mut();
        let Some(s) = guard.as_mut() else {
            return false;
        };
        match msg {
            // Passed on to winit afterwards so the pen still operates egui widgets.
            WM_POINTERDOWN | WM_POINTERUPDATE | WM_POINTERUP if s.api == PenApi::Ink => {
                let id = (wp & 0xFFFF) as u32;
                let mut ty: POINTER_INPUT_TYPE = 0;
                let mut pi: POINTER_PEN_INFO = unsafe { zeroed() };
                unsafe {
                    if GetPointerType(id, &mut ty) == 0
                        || ty != PT_PEN
                        || GetPointerPenInfo(id, &mut pi) == 0
                    {
                        return false;
                    }
                }
                let eraser = pi.penFlags & (PEN_FLAG_ERASER_ | PEN_FLAG_INVERTED_) != 0;
                let barrel = pi.penFlags & PEN_FLAG_BARREL_ != 0;
                let tilt = [pi.tiltX as f32, pi.tiltY as f32];
                let p = normalise_pressure(pi.pressure, 0, INK_MAX);
                push(
                    s,
                    hwnd,
                    pi.pointerInfo.ptPixelLocation,
                    p,
                    eraser,
                    tilt,
                    barrel,
                );
                false
            }
            WM_ACTIVATE => {
                if let Some(w) = &s.wintab
                    && wp & 0xFFFF != 0
                {
                    unsafe { (w.overlap)(w.ctx, 1) };
                }
                false
            }
            m => {
                let Some(w) = &s.wintab else { return false };
                if m != w.msg {
                    return false;
                }
                let mut pk = Packet::default();
                if unsafe { (w.packet)(lp as *mut c_void, wp as u32, &mut pk) } == 0 {
                    return true;
                }
                let p = normalise_pressure(pk.pressure, w.pressure.0, w.pressure.1);
                let tilt = tilt_from_orientation(pk.azimuth, pk.altitude);
                // Bit 0 is the tip; any other button is a barrel switch.
                let barrel = pk.buttons & !1 != 0;
                let eraser = wintab_is_eraser(pk.cursor);
                push(s, hwnd, POINT { x: pk.x, y: pk.y }, p, eraser, tilt, barrel);
                true
            }
        }
    })
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if unsafe { handle(hwnd, msg, wp, lp) } {
        return 0;
    }
    unsafe { DefSubclassProc(hwnd, msg, wp, lp) }
}
