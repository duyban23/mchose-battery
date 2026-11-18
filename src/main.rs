#![windows_subsystem = "windows"]

//! MCHOSE battery reader for Windows. No external crates: direct Win32 FFI.
//!
//! Modes:
//!   (none)      read battery once
//!   --watch     poll in console every 5 s
//!   --tray      system-tray icon, updates tooltip every 5 s
//!   --install   add --tray to HKCU Run (start at logon)
//!   --uninstall remove it

use std::ffi::{c_void, OsString};
use std::io;
use std::mem::{size_of, zeroed, MaybeUninit};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ptr;
use std::thread::sleep;
use std::time::{Duration, Instant};

type Handle = *mut c_void;
const INVALID: Handle = usize::MAX as Handle;
const HKEY_CURRENT_USER: Handle = 0x8000_0001_usize as Handle;

const GENERIC_READ_WRITE: u32 = 0x8000_0000 | 0x4000_0000;
const FILE_SHARE_RW: u32 = 1 | 2;
const OPEN_EXISTING: u32 = 3;
const DIGCF_PRESENT: u32 = 2;
const DIGCF_DEVICEINTERFACE: u32 = 0x10;
const VIDS: [u16; 2] = [0x5253, 0x3837];
const USAGE_PAGE_VENDOR: u16 = 0xFF01;
const REPORT_SHORT: u8 = 0x11;
const SHORT_LEN: u32 = 21;

const WM_TIMER: u32 = 0x0113;
const WM_COMMAND: u32 = 0x0111;
const WM_DESTROY: u32 = 0x0002;
const WM_APP_TRAY: u32 = 0x8000 + 1;
const WM_RBUTTONUP: u32 = 0x0205;
const WM_LBUTTONDBLCLK: u32 = 0x0203;
const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIF_MESSAGE: u32 = 1;
const NIF_ICON: u32 = 2;
const NIF_TIP: u32 = 4;
const TIMER_ID: usize = 1;
const MENU_REFRESH: usize = 1;
const MENU_EXIT: usize = 2;
const MF_STRING: u32 = 0;
const TPM_RIGHTBUTTON: u32 = 2;
const KEY_SET_VALUE: u32 = 0x0002;
const REG_SZ: u32 = 1;
const ERROR_ALREADY_EXISTS: i32 = 183;

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct SpDeviceInterfaceData {
    cb_size: u32,
    guid: Guid,
    flags: u32,
    reserved: usize,
}

#[repr(C)]
#[derive(Default)]
struct HiddAttributes {
    size: u32,
    vendor_id: u16,
    product_id: u16,
    version_number: u16,
}

#[repr(C)]
struct HidpCaps {
    usage: u16,
    usage_page: u16,
    input_len: u16,
    output_len: u16,
    feature_len: u16,
    reserved: [u16; 17],
    counts: [u16; 10],
}

#[repr(C)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[repr(C)]
struct Size {
    cx: i32,
    cy: i32,
}

#[repr(C)]
struct IconInfo {
    icon: i32,
    x_hotspot: u32,
    y_hotspot: u32,
    mask: Handle,
    color: Handle,
}

#[repr(C)]
struct Msg {
    hwnd: Handle,
    message: u32,
    wparam: usize,
    lparam: isize,
    time: u32,
    pt: Point,
    private: u32,
}

#[repr(C)]
struct WndClassW {
    style: u32,
    wnd_proc: Option<extern "system" fn(Handle, u32, usize, isize) -> isize>,
    cls_extra: i32,
    wnd_extra: i32,
    instance: Handle,
    icon: Handle,
    cursor: Handle,
    background: Handle,
    menu_name: *const u16,
    class_name: *const u16,
}

#[repr(C)]
struct NotifyIconDataW {
    cb_size: u32,
    hwnd: Handle,
    uid: u32,
    flags: u32,
    callback_message: u32,
    icon: Handle,
    tip: [u16; 128],
    state: u32,
    state_mask: u32,
    info: [u16; 256],
    timeout_or_version: u32,
    info_title: [u16; 64],
    info_flags: u32,
    guid_item: Guid,
    balloon_icon: Handle,
}

#[link(name = "hid")]
extern "system" {
    fn HidD_GetHidGuid(guid: *mut Guid);
    fn HidD_GetAttributes(handle: Handle, attributes: *mut HiddAttributes) -> i32;
    fn HidD_GetPreparsedData(handle: Handle, preparsed: *mut Handle) -> i32;
    fn HidD_FreePreparsedData(preparsed: Handle) -> i32;
    fn HidD_SetFeature(handle: Handle, buffer: *mut u8, length: u32) -> i32;
    fn HidD_GetFeature(handle: Handle, buffer: *mut u8, length: u32) -> i32;
    fn HidP_GetCaps(preparsed: Handle, caps: *mut HidpCaps) -> i32;
}

#[link(name = "setupapi")]
extern "system" {
    fn SetupDiGetClassDevsW(guid: *const Guid, enumerator: *const u16, parent: Handle, flags: u32) -> Handle;
    fn SetupDiEnumDeviceInterfaces(set: Handle, info: *mut c_void, guid: *const Guid, index: u32, data: *mut SpDeviceInterfaceData) -> i32;
    fn SetupDiGetDeviceInterfaceDetailW(set: Handle, data: *const SpDeviceInterfaceData, detail: *mut u8, size: u32, required: *mut u32, info: *mut c_void) -> i32;
    fn SetupDiDestroyDeviceInfoList(set: Handle) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileW(name: *const u16, access: u32, share: u32, security: *mut c_void, creation: u32, flags: u32, template: Handle) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn GetModuleFileNameW(module: Handle, path: *mut u16, size: u32) -> u32;
    fn GetModuleHandleW(name: *const u16) -> Handle;
    fn CreateMutexW(security: *mut c_void, initial_owner: i32, name: *const u16) -> Handle;
}

#[link(name = "user32")]
extern "system" {
    fn RegisterClassW(class: *const WndClassW) -> u16;
    fn CreateWindowExW(ex: u32, class: *const u16, name: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: Handle, menu: Handle, instance: Handle, param: *mut c_void) -> Handle;
    fn DefWindowProcW(hwnd: Handle, msg: u32, wparam: usize, lparam: isize) -> isize;
    fn DestroyWindow(hwnd: Handle) -> i32;
    fn GetMessageW(msg: *mut Msg, hwnd: Handle, min: u32, max: u32) -> i32;
    fn TranslateMessage(msg: *const Msg) -> i32;
    fn DispatchMessageW(msg: *const Msg) -> isize;
    fn PostQuitMessage(code: i32);
    fn SetTimer(hwnd: Handle, id: usize, elapse: u32, proc: Option<extern "system" fn(Handle, u32, usize, u32)>) -> usize;
    fn KillTimer(hwnd: Handle, id: usize) -> i32;
    fn CreateIconIndirect(info: *const IconInfo) -> Handle;
    fn DestroyIcon(icon: Handle) -> i32;
    fn GetDC(hwnd: Handle) -> Handle;
    fn ReleaseDC(hwnd: Handle, dc: Handle) -> i32;
    fn CreatePopupMenu() -> Handle;
    fn AppendMenuW(menu: Handle, flags: u32, id: usize, text: *const u16) -> i32;
    fn GetCursorPos(point: *mut Point) -> i32;
    fn SetForegroundWindow(hwnd: Handle) -> i32;
    fn TrackPopupMenu(menu: Handle, flags: u32, x: i32, y: i32, reserved: i32, hwnd: Handle, rect: *const c_void) -> i32;
    fn DestroyMenu(menu: Handle) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateCompatibleDC(dc: Handle) -> Handle;
    fn CreateCompatibleBitmap(dc: Handle, width: i32, height: i32) -> Handle;
    fn CreateBitmap(width: i32, height: i32, planes: u32, bits_per_pixel: u32, bits: *const c_void) -> Handle;
    fn SelectObject(dc: Handle, object: Handle) -> Handle;
    fn DeleteObject(object: Handle) -> i32;
    fn DeleteDC(dc: Handle) -> i32;
    fn FillRect(dc: Handle, rect: *const Rect, brush: Handle) -> i32;
    fn CreateSolidBrush(color: u32) -> Handle;
    fn SetTextColor(dc: Handle, color: u32) -> u32;
    fn SetBkMode(dc: Handle, mode: i32) -> i32;
    fn TextOutW(dc: Handle, x: i32, y: i32, text: *const u16, len: i32) -> i32;
    fn GetTextExtentPoint32W(dc: Handle, text: *const u16, len: i32, size: *mut Size) -> i32;
    fn CreateFontW(
        height: i32, width: i32, escapement: i32, orientation: i32, weight: i32,
        italic: u32, underline: u32, strikeout: u32, charset: u32, out_precision: u32,
        clip_precision: u32, quality: u32, pitch: u32, face: *const u16,
    ) -> Handle;
}

#[link(name = "shell32")]
extern "system" {
    fn Shell_NotifyIconW(message: u32, data: *const NotifyIconDataW) -> i32;
}

#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(key: Handle, sub: *const u16, options: u32, access: u32, result: *mut Handle) -> i32;
    fn RegSetValueExW(key: Handle, name: *const u16, reserved: u32, kind: u32, data: *const u8, size: u32) -> i32;
    fn RegDeleteValueW(key: Handle, name: *const u16) -> i32;
    fn RegCloseKey(key: Handle) -> i32;
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn hid_guid() -> Guid {
    let mut g = MaybeUninit::<Guid>::uninit();
    unsafe {
        HidD_GetHidGuid(g.as_mut_ptr());
        g.assume_init()
    }
}

fn interface_paths() -> io::Result<Vec<OsString>> {
    let guid = hid_guid();
    let set = unsafe {
        SetupDiGetClassDevsW(&guid, ptr::null(), ptr::null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE)
    };
    if set == INVALID {
        return Err(last_error());
    }
    let mut result = Vec::new();
    let mut data = SpDeviceInterfaceData {
        cb_size: size_of::<SpDeviceInterfaceData>() as u32,
        guid,
        flags: 0,
        reserved: 0,
    };
    let mut index = 0u32;
    while unsafe { SetupDiEnumDeviceInterfaces(set, ptr::null_mut(), &guid, index, &mut data) } != 0 {
        index += 1;
        let mut required = 0u32;
        unsafe {
            SetupDiGetDeviceInterfaceDetailW(set, &data, ptr::null_mut(), 0, &mut required, ptr::null_mut());
        }
        if required == 0 {
            continue;
        }
        let mut buf = vec![0u8; required as usize];
        buf[..4].copy_from_slice(&8u32.to_ne_bytes());
        if unsafe { SetupDiGetDeviceInterfaceDetailW(set, &data, buf.as_mut_ptr(), required, &mut required, ptr::null_mut()) } == 0 {
            continue;
        }
        let mut chars = Vec::new();
        let mut i = 4;
        while i + 1 < buf.len() {
            let word = u16::from_ne_bytes([buf[i], buf[i + 1]]);
            i += 2;
            if word == 0 {
                break;
            }
            chars.push(word);
        }
        result.push(OsString::from_wide(&chars));
    }
    unsafe { SetupDiDestroyDeviceInfoList(set) };
    Ok(result)
}

struct Device {
    handle: Handle,
    vid: u16,
    pid: u16,
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

fn open_candidates() -> io::Result<Vec<Device>> {
    let mut found = Vec::new();
    for path in interface_paths()? {
        let mut name: Vec<u16> = path.encode_wide().collect();
        name.push(0);
        let handle = unsafe {
            CreateFileW(name.as_ptr(), GENERIC_READ_WRITE, FILE_SHARE_RW, ptr::null_mut(), OPEN_EXISTING, 0, ptr::null_mut())
        };
        if handle == INVALID {
            continue;
        }
        let mut attrs = HiddAttributes {
            size: size_of::<HiddAttributes>() as u32,
            ..Default::default()
        };
        if unsafe { HidD_GetAttributes(handle, &mut attrs) } == 0 || !VIDS.contains(&attrs.vendor_id) {
            unsafe { CloseHandle(handle) };
            continue;
        }
        let mut prepared: Handle = ptr::null_mut();
        if unsafe { HidD_GetPreparsedData(handle, &mut prepared) } == 0 {
            unsafe { CloseHandle(handle) };
            continue;
        }
        let mut caps = MaybeUninit::<HidpCaps>::uninit();
        let ok = unsafe { HidP_GetCaps(prepared, caps.as_mut_ptr()) };
        let caps = if ok == 0 {
            unsafe { HidD_FreePreparsedData(prepared) };
            unsafe { CloseHandle(handle) };
            continue;
        } else {
            unsafe { caps.assume_init() }
        };
        unsafe { HidD_FreePreparsedData(prepared) };
        if caps.usage_page != USAGE_PAGE_VENDOR || u32::from(caps.feature_len) < SHORT_LEN {
            unsafe { CloseHandle(handle) };
            continue;
        }
        found.push(Device { handle, vid: attrs.vendor_id, pid: attrs.product_id });
    }
    Ok(found)
}

#[derive(Debug)]
struct Identity {
    vid: u16,
    pid: u16,
    firmware: u32,
    connect_mode: u8,
    connected: bool,
    battery: u8,
    charging: u8,
}

fn pace(last: &mut Option<Instant>) {
    if let Some(t) = *last {
        let since = t.elapsed();
        if since < Duration::from_millis(25) {
            sleep(Duration::from_millis(25) - since);
        }
    }
    *last = Some(Instant::now());
}

fn read_identity(dev: &Device, last: &mut Option<Instant>) -> io::Result<Identity> {
    for attempt in 0..6 {
        pace(last);
        let mut request = vec![0xFFu8; SHORT_LEN as usize];
        request[0] = REPORT_SHORT;
        request[1] = 0x06 ^ 0xFF;
        if unsafe { HidD_SetFeature(dev.handle, request.as_mut_ptr(), SHORT_LEN) } == 0 {
            return Err(io::Error::new(io::ErrorKind::Other, format!("HidD_SetFeature: {}", last_error())));
        }
        sleep(Duration::from_millis(20));

        let mut reply = vec![0u8; SHORT_LEN as usize];
        let mut got = false;
        for _ in 0..40 {
            pace(last);
            reply.fill(0);
            reply[0] = REPORT_SHORT;
            if unsafe { HidD_GetFeature(dev.handle, reply.as_mut_ptr(), SHORT_LEN) } != 0 {
                got = true;
                break;
            }
            sleep(Duration::from_millis(10));
        }
        if !got {
            return Err(io::Error::new(io::ErrorKind::Other, format!("HidD_GetFeature: {}", last_error())));
        }
        if reply[0] != REPORT_SHORT || (reply[1] ^ 0xFF) != 0x06 {
            sleep(Duration::from_millis(30 * (attempt + 1)));
            continue;
        }
        let body: Vec<u8> = reply[2..].iter().map(|b| b ^ 0xFF).collect();
        if body.iter().all(|b| *b == 0) {
            sleep(Duration::from_millis(30 * (attempt + 1)));
            continue;
        }
        let vid = u16::from_le_bytes([body[0], body[1]]);
        if vid == 0 {
            sleep(Duration::from_millis(120));
            continue;
        }
        let flags = body[8];
        return Ok(Identity {
            vid,
            pid: u16::from_le_bytes([body[2], body[3]]),
            firmware: u32::from_le_bytes([body[4], body[5], body[6], body[7]]),
            connect_mode: flags & 0b111,
            connected: flags & 0b1000 != 0,
            battery: body[9],
            charging: body[10],
        });
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, "no valid identity reply from mouse"))
}

fn probe() -> Option<(String, String, u8)> {
    let mut last = None;
    let devices = open_candidates().ok()?;
    for dev in devices {
        if let Ok(id) = read_identity(&dev, &mut last) {
            if id.connect_mode == 1 && !id.connected {
                continue;
            }
            let link = match id.connect_mode {
                1 => "2.4G",
                2 => "USB",
                3 => "BT",
                _ => "other",
            };
            let charge = if id.charging != 0 { " (charging)" } else { "" };
            let long = format!(
                "battery: {}%{}  link: {}  model: {:04x}:{:04x}  fw: {}",
                id.battery, charge, link, id.vid, id.pid, id.firmware
            );
            let short = format!("MCHOSE {}%{} {}", id.battery, charge, link);
            return Some((long, short, id.battery));
        }
    }
    None
}

static mut TRAY: Option<NotifyIconDataW> = None;
static mut TRAY_ICON: Handle = ptr::null_mut();

// Draw the percentage into a 32x32 icon so it is readable without hovering.
unsafe fn make_icon(percent: Option<u8>) -> Handle {
    let screen = GetDC(ptr::null_mut());
    let dc = CreateCompatibleDC(screen);
    let color = CreateCompatibleBitmap(screen, 32, 32);
    let old_bitmap = SelectObject(dc, color);
    let rect = Rect { left: 0, top: 0, right: 32, bottom: 32 };
    let brush = CreateSolidBrush(0x0050A010); // strong green, high contrast
    FillRect(dc, &rect, brush);
    // CLEARTYPE_QUALITY = 5, bold; short labels stay readable at tray size.
    let font = CreateFontW(22, 0, 0, 0, 700, 0, 0, 0, 0, 0, 0, 5, 0, ptr::null());
    let old_font = SelectObject(dc, font);
    SetTextColor(dc, 0x00FFFFFF);
    SetBkMode(dc, 1); // TRANSPARENT
    let label = match percent {
        Some(p) => p.to_string(),
        None => "?".to_string(),
    };
    let text = wide(&label);
    let mut extent = Size { cx: 0, cy: 0 };
    GetTextExtentPoint32W(dc, text.as_ptr(), (text.len() - 1) as i32, &mut extent);
    let x = (32 - extent.cx) / 2;
    let y = (32 - extent.cy) / 2;
    TextOutW(dc, x, y, text.as_ptr(), (text.len() - 1) as i32);
    let mut mask_bits = [0u8; 128]; // 32x32, 1 bpp, all opaque
    let mask = CreateBitmap(32, 32, 1, 1, mask_bits.as_ptr() as *const c_void);
    let info = IconInfo { icon: 1, x_hotspot: 0, y_hotspot: 0, mask, color };
    let icon = CreateIconIndirect(&info);
    SelectObject(dc, old_font);
    SelectObject(dc, old_bitmap);
    DeleteObject(font);
    DeleteObject(brush);
    DeleteObject(mask);
    DeleteObject(color);
    DeleteDC(dc);
    ReleaseDC(ptr::null_mut(), screen);
    icon
}

fn set_status(text: &str, percent: Option<u8>) {
    unsafe {
        if let Some(ref mut nid) = TRAY {
            let icon = make_icon(percent);
            if icon != ptr::null_mut() {
                nid.icon = icon;
                if TRAY_ICON != ptr::null_mut() {
                    DestroyIcon(TRAY_ICON);
                }
                TRAY_ICON = icon;
            }
            nid.flags = NIF_ICON | NIF_TIP;
            nid.tip.fill(0);
            let chars: Vec<u16> = text.encode_utf16().take(127).collect();
            nid.tip[..chars.len()].copy_from_slice(&chars);
            Shell_NotifyIconW(NIM_MODIFY, nid);
        }
    }
}

fn refresh() {
    match probe() {
        Some((_, short, battery)) => set_status(&short, Some(battery)),
        None => set_status("MCHOSE: no mouse", None),
    }
}

extern "system" fn wnd_proc(hwnd: Handle, message: u32, wparam: usize, lparam: isize) -> isize {
    unsafe {
        match message {
            WM_TIMER if wparam == TIMER_ID => {
                refresh();
                0
            }
            WM_APP_TRAY => {
                match lparam as u32 {
                    WM_RBUTTONUP => {
                        let menu = CreatePopupMenu();
                        AppendMenuW(menu, MF_STRING, MENU_REFRESH, wide("Refresh").as_ptr());
                        AppendMenuW(menu, MF_STRING, MENU_EXIT, wide("Exit").as_ptr());
                        let mut pos = zeroed::<Point>();
                        GetCursorPos(&mut pos);
                        SetForegroundWindow(hwnd);
                        TrackPopupMenu(menu, TPM_RIGHTBUTTON, pos.x, pos.y, 0, hwnd, ptr::null());
                        DestroyMenu(menu);
                    }
                    WM_LBUTTONDBLCLK => refresh(),
                    _ => {}
                }
                0
            }
            WM_COMMAND => {
                match wparam & 0xFFFF {
                    MENU_REFRESH => refresh(),
                    MENU_EXIT => PostQuitMessage(0),
                    _ => {}
                }
                0
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

fn run_tray() {
    unsafe {
        // One tray instance per session: a second launch must not add a duplicate icon.
        let mutex_name = wide("Local\\MCHOSE_Battery_SingleInstance");
        let mutex = CreateMutexW(ptr::null_mut(), 1, mutex_name.as_ptr());
        if mutex == ptr::null_mut() {
            return;
        }
        if io::Error::last_os_error().raw_os_error() == Some(ERROR_ALREADY_EXISTS) {
            CloseHandle(mutex);
            return;
        }
        // Keep the mutex handle for the lifetime of this process.

        let class_name = wide("MCHOSE_BATTERY_TRAY");
        let instance = GetModuleHandleW(ptr::null());
        let class = WndClassW {
            style: 0,
            wnd_proc: Some(wnd_proc),
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            icon: ptr::null_mut(),
            cursor: ptr::null_mut(),
            background: ptr::null_mut(),
            menu_name: ptr::null(),
            class_name: class_name.as_ptr(),
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(0, class_name.as_ptr(), wide("").as_ptr(), 0, 0, 0, 0, 0, ptr::null_mut(), ptr::null_mut(), instance, ptr::null_mut());
        if hwnd == ptr::null_mut() {
            eprintln!("cannot create tray window");
            return;
        }
        let mut nid: NotifyIconDataW = zeroed();
        nid.cb_size = size_of::<NotifyIconDataW>() as u32;
        nid.hwnd = hwnd;
        nid.uid = 1;
        nid.flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        nid.callback_message = WM_APP_TRAY;
        let (initial, percent) = match probe() {
            Some((_, short, battery)) => (short, Some(battery)),
            None => ("MCHOSE: no mouse".to_string(), None),
        };
        let icon = make_icon(percent);
        nid.icon = icon;
        TRAY_ICON = icon;
        let chars: Vec<u16> = initial.encode_utf16().take(127).collect();
        nid.tip[..chars.len()].copy_from_slice(&chars);
        TRAY = Some(nid);
        let added = Shell_NotifyIconW(NIM_ADD, TRAY.as_ref().unwrap());
        if std::env::var_os("MCHOSE_DEBUG").is_some() {
            eprintln!("NIM_ADD={} tip={:?}", added, initial);
        }
        SetTimer(hwnd, TIMER_ID, 5000, None);

        let mut msg = zeroed::<Msg>();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if let Some(ref mut nid) = TRAY {
            Shell_NotifyIconW(NIM_DELETE, nid);
        }
        if TRAY_ICON != ptr::null_mut() {
            DestroyIcon(TRAY_ICON);
        }
        KillTimer(hwnd, TIMER_ID);
        DestroyWindow(hwnd);
    }
}

fn set_autostart(install: bool) -> io::Result<()> {
    let mut path = vec![0u16; 512];
    let len = unsafe { GetModuleFileNameW(ptr::null_mut(), path.as_mut_ptr(), path.len() as u32) } as usize;
    if len == 0 {
        return Err(last_error());
    }
    path.truncate(len);
    let exe = OsString::from_wide(&path).to_string_lossy().into_owned();
    let command = wide(&format!("\"{}\" --tray", exe));
    let sub = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let name = wide("MCHOSE Battery");
    let mut key: Handle = ptr::null_mut();
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_SET_VALUE, &mut key) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let rc = if install {
        unsafe { RegSetValueExW(key, name.as_ptr(), 0, REG_SZ, command.as_ptr() as *const u8, (command.len() * 2) as u32) }
    } else {
        unsafe { RegDeleteValueW(key, name.as_ptr()) }
    };
    unsafe { RegCloseKey(key) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--install") {
        match set_autostart(true) {
            Ok(()) => println!("autostart enabled"),
            Err(e) => eprintln!("autostart failed: {}", e),
        }
        return;
    }
    if args.iter().any(|a| a == "--uninstall") {
        match set_autostart(false) {
            Ok(()) => println!("autostart disabled"),
            Err(e) => eprintln!("autostart failed: {}", e),
        }
        return;
    }
    if args.iter().any(|a| a == "--tray") {
        run_tray();
        return;
    }
    let watch = args.iter().any(|a| a == "--watch" || a == "-w");
    loop {
        match probe() {
            Some((long, _, _)) => println!("{}", long),
            None => eprintln!("no responding MCHOSE mouse found"),
        }
        if !watch {
            break;
        }
        sleep(Duration::from_secs(5));
    }
}
