//! Win32 pen reading (WinTab packets or Ink `WM_POINTER`) into `PenEvent`s; from `spikes/pen-test`.

use crate::input::{
    PenApi, PenEvent, PhaseTracker, WintabState, active_api, contact, normalise_pressure,
    tilt_from_orientation, wintab_is_eraser,
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
/// Ink coalesces moves; its history holds the samples in between.
const INK_HISTORY: usize = 64;

// wintab.h. Packet field order follows the bit order of PK_DATA.
const WTI_DEFSYSCTX: u32 = 4;
const WTI_DEVICES: u32 = 100;
const DVC_NAME: u32 = 1;
const DVC_NPRESSURE: u32 = 15;
const CXO_MESSAGES: u32 = 0x0004;
const PK_DATA: u32 = 0x0002 | 0x0004 | 0x0020 | 0x0040 | 0x0080 | 0x0100 | 0x0400 | 0x1000;
/// Packet status bit: packets were lost because the queue was full.
const TPS_QUEUE_ERR: u32 = 0x0002;
/// Queue sizes tried in order; the driver default of about 8 packets overflows in one slow frame.
const QUEUE_SIZES: [i32; 7] = [1024, 512, 256, 128, 64, 32, 16];
const PACKET_BATCH: usize = 64;

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
#[derive(Clone, Copy, Default)]
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
type WtPackets = unsafe extern "system" fn(*mut c_void, i32, *mut Packet) -> i32;
type WtQueueSize = unsafe extern "system" fn(*mut c_void, i32) -> BOOL;
type WtCtx = unsafe extern "system" fn(*mut c_void, BOOL) -> BOOL;

struct Wintab {
    packets: WtPackets,
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
    /// Packets marked as after a WinTab queue overflow, since the last `drain`.
    lost: u32,
}

thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

/// Which API ended up active, and the WinTab device or why WinTab is not used.
pub struct Status {
    pub api: PenApi,
    pub wintab: WintabState,
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
        (None, WintabState::NotSelected)
    };
    let api = active_api(setting, wintab.is_some());
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            api,
            wintab,
            phase: PhaseTracker::default(),
            queue: Vec::new(),
            repaint,
            lost: 0,
        })
    });
    unsafe { SetWindowSubclass(hwnd, Some(subclass), 1, 0) };
    Status { api, wintab: note }
}

/// Takes the pen events that arrived since the last call; logs WinTab queue overflows.
pub fn drain() -> Vec<PenEvent> {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(s) = s.as_mut() else {
            return Vec::new();
        };
        if s.lost > 0 {
            log::warn!("wintab queue overflowed ({} packets flagged)", s.lost);
            s.lost = 0;
        }
        std::mem::take(&mut s.queue)
    })
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// The WinTab context and the device it opened, or why it could not.
unsafe fn load_wintab(hwnd: HWND) -> Result<(Wintab, WintabState), WintabState> {
    unsafe {
        let lib = LoadLibraryW(wide("Wintab32.dll").as_ptr());
        if lib.is_null() {
            return Err(WintabState::NoDriver);
        }
        let f = |n: &[u8]| GetProcAddress(lib, n.as_ptr()).ok_or(WintabState::BadDriver);
        let info: WtInfo = std::mem::transmute(f(b"WTInfoW\0")?);
        let open: WtOpen = std::mem::transmute(f(b"WTOpenW\0")?);
        let packets: WtPackets = std::mem::transmute(f(b"WTPacketsGet\0")?);
        let queue_size: WtQueueSize = std::mem::transmute(f(b"WTQueueSizeSet\0")?);
        let overlap: WtCtx = std::mem::transmute(f(b"WTOverlap\0")?);
        if info(0, 0, null_mut()) == 0 {
            return Err(WintabState::NoService);
        }

        let mut name = [0u16; 128];
        info(WTI_DEVICES, DVC_NAME, name.as_mut_ptr().cast());
        let len = name.iter().position(|&c| c == 0).unwrap_or(0);
        let device = String::from_utf16_lossy(&name[..len]);
        let mut axis = Axis::default();
        info(WTI_DEVICES, DVC_NPRESSURE, (&mut axis as *mut Axis).cast());

        let mut lc: LogContext = zeroed();
        if info(WTI_DEFSYSCTX, 0, (&mut lc as *mut LogContext).cast()) == 0 {
            return Err(WintabState::NoContext);
        }
        lc.options |= CXO_MESSAGES;
        lc.pkt_data = PK_DATA;
        // Absolute mode: `buttons` is the state of every button, bit 0 the tip.
        lc.pkt_mode = 0;
        lc.move_mask = PK_DATA;
        // Output in screen pixels; the negative Y extent flips WinTab's Y-up origin.
        lc.out_org = [lc.sys_org_x, lc.sys_org_y, 0];
        lc.out_ext = [lc.sys_ext_x, -lc.sys_ext_y, lc.out_ext[2]];
        let ctx = open(hwnd, &mut lc, 1);
        if ctx.is_null() {
            return Err(WintabState::NoContext);
        }
        let queue = QUEUE_SIZES.into_iter().find(|&n| queue_size(ctx, n) != 0);
        log::info!("wintab queue size {queue:?}");
        let pressure = (axis.min.max(0) as u32, axis.max.max(1) as u32);
        let device = WintabState::Device {
            name: device,
            pressure,
        };
        Ok((
            Wintab {
                packets,
                overlap,
                ctx,
                msg: lc.msg_base,
                pressure,
            },
            device,
        ))
    }
}

/// One sample in screen pixels; `tip` is the driver's contact state.
struct Raw {
    screen: POINT,
    pressure: f32,
    tip: bool,
    eraser: bool,
    tilt: [f32; 2],
    barrel: bool,
}

fn push(s: &mut State, hwnd: HWND, r: Raw) {
    let mut pt = r.screen;
    unsafe { ScreenToClient(hwnd, &mut pt) };
    let phase = s.phase.next(contact(r.tip, r.pressure));
    s.queue.push(PenEvent {
        pos: [pt.x as f32, pt.y as f32],
        pressure: r.pressure,
        eraser: r.eraser,
        tilt: r.tilt,
        barrel: r.barrel,
        phase,
    });
    s.repaint.request_repaint();
}

/// Returns true when the message is consumed.
unsafe fn handle(hwnd: HWND, msg: u32, wp: WPARAM, _lp: LPARAM) -> bool {
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
                let mut infos: [POINTER_PEN_INFO; INK_HISTORY] = unsafe { zeroed() };
                let mut n = INK_HISTORY as u32;
                unsafe {
                    if GetPointerType(id, &mut ty) == 0
                        || ty != PT_PEN
                        || GetPointerPenInfoHistory(id, &mut n, infos.as_mut_ptr()) == 0
                    {
                        return false;
                    }
                }
                // History is newest first.
                for pi in infos[..(n as usize).min(INK_HISTORY)].iter().rev() {
                    let raw = Raw {
                        screen: pi.pointerInfo.ptPixelLocation,
                        pressure: normalise_pressure(pi.pressure, 0, INK_MAX),
                        tip: pi.pointerInfo.pointerFlags & POINTER_FLAG_INCONTACT != 0,
                        eraser: pi.penFlags & (PEN_FLAG_ERASER_ | PEN_FLAG_INVERTED_) != 0,
                        tilt: [pi.tiltX as f32, pi.tiltY as f32],
                        barrel: pi.penFlags & PEN_FLAG_BARREL_ != 0,
                    };
                    push(s, hwnd, raw);
                }
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
                let (packets, ctx, range) = (w.packets, w.ctx, w.pressure);
                // One message may stand for many queued packets; take them all, oldest first.
                let mut buf = [Packet::default(); PACKET_BATCH];
                loop {
                    let n = unsafe { packets(ctx, PACKET_BATCH as i32, buf.as_mut_ptr()) };
                    let n = n.clamp(0, PACKET_BATCH as i32) as usize;
                    for pk in &buf[..n] {
                        if pk.status & TPS_QUEUE_ERR != 0 {
                            s.lost += 1;
                        }
                        // Bit 0 is the tip; any other button is a barrel switch.
                        let raw = Raw {
                            screen: POINT { x: pk.x, y: pk.y },
                            pressure: normalise_pressure(pk.pressure, range.0, range.1),
                            tip: pk.buttons & 1 != 0,
                            eraser: wintab_is_eraser(pk.cursor),
                            tilt: tilt_from_orientation(pk.azimuth, pk.altitude),
                            barrel: pk.buttons & !1 != 0,
                        };
                        push(s, hwnd, raw);
                    }
                    if n < PACKET_BATCH {
                        break;
                    }
                }
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
