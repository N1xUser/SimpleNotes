//! Win32 UI. All windows and SQLite access live on the message-loop thread.
//! Window data is owned from WM_NCCREATE through WM_NCDESTROY; interior cells
//! allow synchronous, reentrant Windows messages without RefCell borrow conflicts.
use crate::storage::{Note, Store, TodoItem};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_void,
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::{null, null_mut},
    rc::Rc,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    System::{LibraryLoader::*, Registry::*},
    UI::{
        Accessibility::*, Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*,
        WindowsAndMessaging::*,
    },
};

const APP: &str = "SimpleNotes";
const NOTE_CLASS: &str = "SimpleNotes.Note.1";
const HOST_CLASS: &str = "SimpleNotes.Host.1";
const PANEL_CLASS: &str = "SimpleNotes.Panel.1";
const WM_TRAY: u32 = WM_APP + 1;
const WM_LOWER: u32 = WM_APP + 2;
const WM_SCALE: u32 = WM_APP + 3;
const CMD_NEW: usize = 100;
const CMD_LIBRARY: usize = 101;
const CMD_SETTINGS: usize = 102;
const CMD_HIDE: usize = 103;
const CMD_BACKUP: usize = 104;
const CMD_EXIT: usize = 105;
const CMD_EXPORT: usize = 106;
const CMD_FOLDER: usize = 107;
const CMD_ARCHIVE: usize = 108;
const CMD_TOGGLE_TODO: usize = 109;
const CMD_UNDO: usize = 110;
const CMD_CUT: usize = 111;
const CMD_COPY: usize = 112;
const CMD_PASTE: usize = 113;
const CMD_SELECT_ALL: usize = 114;
const WM_CHROME_HOVER: u32 = WM_APP + 8;

const ID_EDIT: usize = 200;
const ID_STARTUP: usize = 201;
const ID_SCALE: usize = 202;
const ID_SCALE_VALUE: usize = 207;
const ID_LIST: usize = 203;
const ID_OPEN: usize = 204;
const ID_TODO_LIST: usize = 210;
const ID_TODO_INPUT: usize = 211;
const ID_TODO_ADD: usize = 212;
const WM_TODO_ACTION: u32 = WM_APP + 6;

const SCALE_MIN: u32 = 75;
const SCALE_MAX: u32 = 200;
const TBM_GETPOS: u32 = WM_USER;
const BG: u32 = 0x00303030;
const FG: u32 = 0x00eeeeee;
const ACCENT: u32 = 0x0048cbe9;
const PANEL_BG: u32 = 0x00222222;
const CONTROL_BG: u32 = 0x00353535;
thread_local! { static HOST: Cell<HWND> = const { Cell::new(null_mut()) }; }
struct MenuLabel {
    text: String,
    submenu: bool,
}
thread_local! {
    static MENU_LABELS:RefCell<HashMap<usize,MenuLabel>>=RefCell::new(HashMap::new());
    static MENU_KEY:Cell<usize>=const{Cell::new(1)};
    static MENU_BRUSH:Cell<HBRUSH>=const{Cell::new(null_mut())};
}

unsafe fn create_dark_menu() -> HMENU {
    let menu = CreatePopupMenu();
    let brush = MENU_BRUSH.with(|b| {
        if b.get().is_null() {
            b.set(CreateSolidBrush(PANEL_BG));
        }
        b.get()
    });
    let mut info: MENUINFO = zeroed();
    info.cbSize = size_of::<MENUINFO>() as u32;
    info.fMask = MIM_BACKGROUND;
    info.hbrBack = brush;
    SetMenuInfo(menu, &info);
    menu
}

unsafe fn append_dark_menu(menu: HMENU, flags: u32, id: usize, label: *const u16) -> i32 {
    let mut count = 0;
    if !label.is_null() {
        while *label.add(count) != 0 {
            count += 1;
        }
    }
    let text = if count == 0 {
        String::new()
    } else {
        String::from_utf16_lossy(std::slice::from_raw_parts(label, count))
    };
    let key = MENU_KEY.with(|k| {
        let next = k.get();
        k.set(next + 1);
        next
    });
    MENU_LABELS.with(|labels| {
        labels.borrow_mut().insert(
            key,
            MenuLabel {
                text,
                submenu: flags & MF_POPUP != 0,
            },
        )
    });
    AppendMenuW(
        menu,
        (flags & !MF_SEPARATOR)
            | MF_OWNERDRAW
            | if flags & MF_SEPARATOR != 0 {
                MF_DISABLED
            } else {
                0
            },
        if flags & MF_SEPARATOR != 0 { 0 } else { id },
        key as *const u16,
    )
}

unsafe fn destroy_dark_menu(menu: HMENU) {
    unsafe fn clean(menu: HMENU) {
        for index in 0..GetMenuItemCount(menu) {
            let sub = GetSubMenu(menu, index);
            if !sub.is_null() {
                clean(sub);
            }
            let mut info: MENUITEMINFOW = zeroed();
            info.cbSize = size_of::<MENUITEMINFOW>() as u32;
            info.fMask = MIIM_DATA;
            if GetMenuItemInfoW(menu, index as u32, 1, &mut info) != 0 {
                MENU_LABELS.with(|labels| labels.borrow_mut().remove(&info.dwItemData));
            }
        }
    }
    clean(menu);
    DestroyMenu(menu);
}

unsafe fn dark_menu_measure(hwnd: HWND, l: LPARAM) -> bool {
    let item = &mut *(l as *mut MEASUREITEMSTRUCT);
    if item.CtlType != ODT_MENU {
        return false;
    }
    MENU_LABELS.with(|labels| {
        if let Some(label) = labels.borrow().get(&item.itemData) {
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let dc = GetDC(hwnd);
            let f = font(14 * dpi / 96);
            let old = SelectObject(dc, f);
            let mut size: SIZE = zeroed();
            let labeltext = wide(&label.text);
            GetTextExtentPoint32W(
                dc,
                labeltext.as_ptr(),
                (labeltext.len() - 1) as i32,
                &mut size,
            );
            SelectObject(dc, old);
            DeleteObject(f);
            ReleaseDC(hwnd, dc);
            item.itemWidth = (size.cx + 44 * dpi / 96).max(180 * dpi / 96) as u32;
            item.itemHeight = if label.text.is_empty() {
                8 * dpi / 96
            } else {
                30 * dpi / 96
            } as u32;
        }
    });
    true
}

unsafe fn dark_menu_draw(hwnd: HWND, l: LPARAM) -> bool {
    let item = &*(l as *const DRAWITEMSTRUCT);
    if item.CtlType != ODT_MENU {
        return false;
    }
    MENU_LABELS.with(|labels| {
        if let Some(label) = labels.borrow().get(&item.itemData) {
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let dc = item.hDC;
            let brush = CreateSolidBrush(if item.itemState & ODS_SELECTED != 0 {
                0x00404040
            } else {
                PANEL_BG
            });
            FillRect(dc, &item.rcItem, brush);
            DeleteObject(brush);
            if !label.text.is_empty() {
                let f = font(14 * dpi / 96);
                let old = SelectObject(dc, f);
                SetBkMode(dc, TRANSPARENT as i32);
                SetTextColor(
                    dc,
                    if item.itemState & ODS_DISABLED != 0 {
                        0x00888888
                    } else {
                        FG
                    },
                );
                let mut r = item.rcItem;
                r.left += 12 * dpi / 96;
                r.right -= 22 * dpi / 96;
                DrawTextW(
                    dc,
                    wide(&label.text).as_ptr(),
                    -1,
                    &mut r,
                    DT_LEFT | DT_VCENTER | DT_SINGLELINE,
                );
                if label.submenu {
                    r.left = r.right;
                    r.right = item.rcItem.right - 4 * dpi / 96;
                    DrawTextW(
                        dc,
                        wide("›").as_ptr(),
                        -1,
                        &mut r,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );
                }
                SelectObject(dc, old);
                DeleteObject(f);
            }
        }
    });
    true
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn lo(v: usize) -> usize {
    v & 0xffff
}
fn hi(v: usize) -> u32 {
    ((v >> 16) & 0xffff) as u32
}
unsafe fn message(hwnd: HWND, text: &str, flags: u32) -> i32 {
    dark_message(hwnd, text, flags & MB_YESNO != 0)
}

struct DarkMessage {
    text: String,
    yesno: bool,
    result: Cell<i32>,
    font: HFONT,
    brush: HBRUSH,
    dpi: i32,
    height: i32,
}
thread_local! {static DIALOG_REGISTERED:Cell<bool>=const{Cell::new(false)};}

unsafe fn dark_message(owner: HWND, text: &str, yesno: bool) -> i32 {
    let dpi = if owner.is_null() {
        GetDpiForSystem()
    } else {
        GetDpiForWindow(owner)
    }
    .max(96) as i32;
    let scale = |n: i32| n * dpi / 96;
    let f = font(scale(15));
    let dc = GetDC(owner);
    let old = SelectObject(dc, f);
    let mut content = RECT {
        left: 0,
        top: 0,
        right: scale(400),
        bottom: 0,
    };
    DrawTextW(
        dc,
        wide(text).as_ptr(),
        -1,
        &mut content,
        DT_CALCRECT | DT_WORDBREAK,
    );
    SelectObject(dc, old);
    ReleaseDC(owner, dc);
    let data = Box::new(DarkMessage {
        text: text.into(),
        yesno,
        result: Cell::new(0),
        font: f,
        brush: CreateSolidBrush(PANEL_BG),
        dpi,
        height: (content.bottom + scale(102))
            .min(scale(650))
            .max(scale(170)),
    });
    let class = wide("SimpleNotes.Dialog.1");
    DIALOG_REGISTERED.with(|registered| {
        if !registered.get() {
            let mut wc: WNDCLASSEXW = zeroed();
            wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
            wc.lpfnWndProc = Some(dark_message_proc);
            wc.hInstance = module();
            wc.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
            wc.lpszClassName = class.as_ptr();
            registered.set(RegisterClassExW(&wc) != 0);
        }
    });
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: scale(448),
        bottom: data.height,
    };
    AdjustWindowRectExForDpi(&mut rect, style, 0, WS_EX_TOOLWINDOW, dpi as u32);
    let work = work_area(MonitorFromWindow(owner, MONITOR_DEFAULTTOPRIMARY));
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let dialog = CreateWindowExW(
        WS_EX_TOOLWINDOW,
        class.as_ptr(),
        wide(APP).as_ptr(),
        style,
        work.left + (work.right - work.left - width) / 2,
        work.top + (work.bottom - work.top - height) / 2,
        width,
        height,
        owner,
        null_mut(),
        module(),
        &*data as *const _ as *const c_void,
    );
    if dialog.is_null() {
        DeleteObject(data.font);
        DeleteObject(data.brush);
        return if yesno { IDNO } else { IDOK };
    }
    let was_enabled = !owner.is_null() && IsWindowEnabled(owner) != 0;
    if was_enabled {
        EnableWindow(owner, 0);
    }
    ShowWindow(dialog, SW_SHOW);
    SetForegroundWindow(dialog);
    SetFocus(GetDlgItem(dialog, if yesno { IDNO } else { IDOK }));
    let mut msg: MSG = zeroed();
    while IsWindow(dialog) != 0 {
        let result = GetMessageW(&mut msg, null_mut(), 0, 0);
        if result <= 0 {
            if result == 0 {
                PostQuitMessage(msg.wParam as i32);
            }
            DestroyWindow(dialog);
            break;
        }
        if IsDialogMessageW(dialog, &msg) == 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    if was_enabled && IsWindow(owner) != 0 {
        EnableWindow(owner, 1);
    }
    DeleteObject(data.font);
    DeleteObject(data.brush);
    let result = data.result.get();
    if result == 0 {
        if yesno {
            IDNO
        } else {
            IDOK
        }
    } else {
        result
    }
}

unsafe extern "system" fn dark_message_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
        );
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const DarkMessage;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let data = &*ptr;
    let scale = |n: i32| n * data.dpi / 96;
    match msg {
        WM_CREATE => {
            dark_title_bar(hwnd);
            let label = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                wide(&data.text).as_ptr(),
                WS_CHILD | WS_VISIBLE,
                scale(24),
                scale(24),
                scale(400),
                data.height - scale(94),
                hwnd,
                null_mut(),
                module(),
                null(),
            );
            SendMessageW(label, WM_SETFONT, data.font as usize, 1);
            let buttons = if data.yesno {
                vec![(IDYES, "Yes"), (IDNO, "No")]
            } else {
                vec![(IDOK, "OK")]
            };
            let count = buttons.len() as i32;
            for (index, (id, title)) in buttons.into_iter().enumerate() {
                let button = CreateWindowExW(
                    0,
                    wide("BUTTON").as_ptr(),
                    wide(title).as_ptr(),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_TABSTOP
                        | if index == count as usize - 1 {
                            BS_DEFPUSHBUTTON as u32
                        } else {
                            0
                        },
                    scale(424 - (count - index as i32) * 108),
                    data.height - scale(54),
                    scale(96),
                    scale(32),
                    hwnd,
                    id as HMENU,
                    module(),
                    null(),
                );
                SendMessageW(button, WM_SETFONT, data.font as usize, 1);
                SetWindowSubclass(button, Some(dark_control_proc), id as usize, 0);
            }
            return 0;
        }
        WM_ERASEBKGND => {
            FillRect(w as HDC, &client(hwnd), data.brush);
            return 1;
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            SetTextColor(w as HDC, FG);
            SetBkColor(w as HDC, PANEL_BG);
            return data.brush as isize;
        }
        WM_COMMAND if matches!(lo(w) as i32, IDYES | IDNO | IDOK | IDCANCEL) => {
            data.result.set(if lo(w) as i32 == IDCANCEL {
                if data.yesno {
                    IDNO
                } else {
                    IDOK
                }
            } else {
                lo(w) as i32
            });
            DestroyWindow(hwnd);
            return 0;
        }
        WM_CLOSE => {
            data.result.set(if data.yesno { IDNO } else { IDOK });
            DestroyWindow(hwnd);
            return 0;
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
        _ => (),
    }
    DefWindowProcW(hwnd, msg, w, l)
}
unsafe fn error(hwnd: HWND, error: impl std::fmt::Display) {
    message(
        hwnd,
        &format!("The operation could not be completed.\n\n{error}"),
        MB_OK | MB_ICONERROR,
    );
}
unsafe fn module() -> HINSTANCE {
    GetModuleHandleW(null())
}

unsafe fn dark_title_bar(hwnd: HWND) {
    let enabled: i32 = 1;
    DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
        &enabled as *const _ as *const c_void,
        size_of::<i32>() as u32,
    );
    for (attribute, color) in [
        (DWMWA_CAPTION_COLOR, PANEL_BG),
        (DWMWA_TEXT_COLOR, FG),
        (DWMWA_BORDER_COLOR, PANEL_BG),
    ] {
        DwmSetWindowAttribute(
            hwnd,
            attribute as u32,
            &color as *const _ as *const c_void,
            size_of::<u32>() as u32,
        );
    }
}

// Paint the native checkbox and combo while retaining their keyboard behavior,
// accessibility roles, selection, and notifications. No replacement widget tree.
unsafe fn paint_dark_control(hwnd: HWND, dc: HDC, id: usize) {
    let r = client(hwnd);
    let scale = |n: i32| (n * GetDpiForWindow(hwnd) as i32 / 96).max(1);
    let background = CreateSolidBrush(if matches!(id, ID_STARTUP | ID_SCALE) {
        PANEL_BG
    } else {
        CONTROL_BG
    });
    FillRect(dc, &r, background);
    DeleteObject(background);
    let previous_font = SelectObject(dc, SendMessageW(hwnd, WM_GETFONT, 0, 0) as HGDIOBJ);
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(
        dc,
        if IsWindowEnabled(hwnd) != 0 {
            FG
        } else {
            0x00888888
        },
    );
    let mut label = r;
    if id == ID_SCALE {
        let mut channel: RECT = zeroed();
        let mut thumb: RECT = zeroed();
        SendMessageW(hwnd, TBM_GETCHANNELRECT, 0, &mut channel as *mut _ as isize);
        SendMessageW(hwnd, TBM_GETTHUMBRECT, 0, &mut thumb as *mut _ as isize);
        let y = (thumb.top + thumb.bottom) / 2;
        let x = (thumb.left + thumb.right) / 2;
        let track = RECT {
            left: channel.left,
            top: y - scale(2),
            right: channel.right,
            bottom: y + scale(2),
        };
        let brush = CreateSolidBrush(0x00585858);
        FillRect(dc, &track, brush);
        DeleteObject(brush);
        let fill = RECT { right: x, ..track };
        let brush = CreateSolidBrush(ACCENT);
        FillRect(dc, &fill, brush);
        let oldbrush = SelectObject(dc, brush);
        let oldpen = SelectObject(dc, GetStockObject(NULL_PEN));
        let radius = scale(7);
        Ellipse(dc, x - radius, y - radius, x + radius, y + radius);
        SelectObject(dc, oldpen);
        SelectObject(dc, oldbrush);
        DeleteObject(brush);
        SelectObject(dc, previous_font);
        return;
    }
    if id == ID_STARTUP {
        let size = scale(16);
        let top = (r.bottom - size) / 2;
        let square = RECT {
            left: 0,
            top,
            right: size,
            bottom: top + size,
        };
        let checked = SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
        let brush = CreateSolidBrush(if checked { ACCENT } else { CONTROL_BG });
        FillRect(dc, &square, brush);
        DeleteObject(brush);
        let border = CreateSolidBrush(if checked { ACCENT } else { 0x00808080 });
        FrameRect(dc, &square, border);
        DeleteObject(border);
        if checked {
            let pen = CreatePen(PS_SOLID, scale(2), PANEL_BG);
            let old = SelectObject(dc, pen);
            MoveToEx(dc, scale(3), top + scale(8), null_mut());
            LineTo(dc, scale(7), top + scale(12));
            LineTo(dc, scale(13), top + scale(4));
            SelectObject(dc, old);
            DeleteObject(pen);
        }
        label.left = scale(26);
    } else {
        let border = CreateSolidBrush(if GetFocus() == hwnd {
            ACCENT
        } else {
            0x00585858
        });
        FrameRect(dc, &r, border);
        DeleteObject(border);
    }
    DrawTextW(
        dc,
        wide(&text(hwnd)).as_ptr(),
        -1,
        &mut label,
        if id == ID_STARTUP { DT_LEFT } else { DT_CENTER } | DT_VCENTER | DT_SINGLELINE,
    );
    if id == ID_STARTUP
        && GetFocus() == hwnd
        && SendMessageW(hwnd, WM_QUERYUISTATE, 0, 0) & UISF_HIDEFOCUS as isize == 0
    {
        DrawFocusRect(dc, &label);
    }
    SelectObject(dc, previous_font);
}

unsafe extern "system" fn dark_control_proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            let mut ps = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            paint_dark_control(hwnd, dc, id);
            EndPaint(hwnd, &ps);
            return 0;
        }
        WM_PRINTCLIENT => {
            paint_dark_control(hwnd, w as HDC, id);
            return 0;
        }
        WM_CTLCOLORLISTBOX => {
            // Forward the combo's popup-list colors to its parent panel.
            return SendMessageW(GetParent(hwnd), msg, w, l);
        }
        WM_NCDESTROY => {
            RemoveWindowSubclass(hwnd, Some(dark_control_proc), id);
        }
        _ => (),
    }
    let result = DefSubclassProc(hwnd, msg, w, l);
    if matches!(
        msg,
        BM_SETCHECK
            | BM_SETSTATE
            | CB_SETCURSEL
            | TBM_SETPOS
            | WM_SETFOCUS
            | WM_KILLFOCUS
            | WM_ENABLE
            | WM_THEMECHANGED
            | WM_LBUTTONUP
            | WM_KEYUP
            | WM_UPDATEUISTATE
    ) {
        InvalidateRect(hwnd, null(), 0);
    }
    result
}
unsafe fn font(px: i32) -> HFONT {
    CreateFontW(
        -px,
        0,
        0,
        0,
        400,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        DEFAULT_PITCH as u32,
        wide("Segoe UI").as_ptr(),
    )
}
unsafe fn client(hwnd: HWND) -> RECT {
    let mut r = zeroed();
    GetClientRect(hwnd, &mut r);
    r
}
unsafe fn text(hwnd: HWND) -> String {
    let mut buffer = vec![0u16; GetWindowTextLengthW(hwnd) as usize + 1];
    let count = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
    String::from_utf16_lossy(&buffer[..count.max(0) as usize])
}
unsafe fn bottom(hwnd: HWND) {
    SetWindowPos(
        hwnd,
        HWND_BOTTOM,
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
    );
}
unsafe fn open_folder(hwnd: HWND, path: &std::path::Path) {
    let result = ShellExecuteW(
        hwnd,
        wide("open").as_ptr(),
        wide(&path.to_string_lossy()).as_ptr(),
        null(),
        null(),
        SW_SHOWNORMAL,
    );
    if result as usize <= 32 {
        error(hwnd, "Could not open the folder.");
    }
}

struct App {
    store: Rc<Store>,
    host: Cell<HWND>,
    windows: RefCell<HashMap<i64, HWND>>,
    settings: Cell<HWND>,
    library: Cell<HWND>,
    scale: Cell<u32>,
    hidden: Cell<bool>,
    icon: HICON,
    taskbar_message: u32,
    inspect_windows: bool,
}

impl App {
    unsafe fn handles(&self) -> Vec<HWND> {
        self.windows.borrow().values().copied().collect()
    }
    unsafe fn lower(&self) {
        for hwnd in self.handles() {
            bottom(hwnd);
        }
    }
    unsafe fn save_all(&self) -> bool {
        let mut ok = true;
        for hwnd in self.handles() {
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const NoteWindow;
            if !ptr.is_null() && !(*ptr).save(hwnd) {
                ok = false;
            }
        }
        ok
    }
    unsafe fn refresh_library(&self) {
        if !self.library.get().is_null() {
            PostMessageW(self.library.get(), WM_APP + 10, 0, 0);
        }
    }
    unsafe fn create_note(self: &Rc<Self>, content: &str) {
        let offset = (self.windows.borrow().len() % 12) as i32 * 24;
        let mut point = POINT { x: 0, y: 0 };
        GetCursorPos(&mut point);
        let work = work_area(MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY));
        match self
            .store
            .create(content, work.left + 48 + offset, work.top + 48 + offset)
        {
            Ok(n) => {
                self.hidden.set(false);
                self.show_note(n);
                self.refresh_library();
            }
            Err(e) => error(self.host.get(), e),
        }
    }
    unsafe fn show_note(self: &Rc<Self>, note: Note) {
        let existing = self.windows.borrow().get(&note.id).copied();
        if let Some(hwnd) = existing {
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            bottom(hwnd);
            return;
        }
        let data = NoteWindow::new(self.clone(), &note);
        let rect = visible_rect(&note);
        let hwnd = CreateWindowExW(
            if self.inspect_windows {
                WS_EX_APPWINDOW
            } else {
                WS_EX_TOOLWINDOW
            },
            wide(NOTE_CLASS).as_ptr(),
            wide(&format!("Note {} · {APP}", note.id)).as_ptr(),
            WS_POPUP | WS_THICKFRAME | WS_CLIPCHILDREN,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            null_mut(),
            null_mut(),
            module(),
            Box::into_raw(data) as *const c_void,
        );
        if hwnd.is_null() {
            error(
                self.host.get(),
                "Could not open the note window. Its contents are still saved.",
            );
            return;
        }
        self.windows.borrow_mut().insert(note.id, hwnd);
        ShowWindow(
            hwnd,
            if self.hidden.get() {
                SW_HIDE
            } else {
                SW_SHOWNOACTIVATE
            },
        );
        bottom(hwnd);
    }
    unsafe fn command(self: &Rc<Self>, id: usize) {
        match id {
            CMD_NEW => self.create_note(""),
            CMD_SETTINGS => show_panel(self.clone(), PanelKind::Settings),
            CMD_LIBRARY => show_panel(self.clone(), PanelKind::Library),
            CMD_HIDE => {
                self.hidden.set(!self.hidden.get());
                for hwnd in self.handles() {
                    ShowWindow(
                        hwnd,
                        if self.hidden.get() {
                            SW_HIDE
                        } else {
                            SW_SHOWNOACTIVATE
                        },
                    );
                }
                self.lower();
            }
            CMD_BACKUP => {
                if self.save_all() {
                    match self.store.backup(true) {
                        Ok(Some(p)) => {
                            message(
                                self.host.get(),
                                &format!("Backup created:\n{}", p.display()),
                                MB_OK | MB_ICONINFORMATION,
                            );
                        }
                        Ok(None) => (),
                        Err(e) => error(self.host.get(), e),
                    }
                }
            }
            CMD_EXPORT => {
                if self.save_all() {
                    match self.store.export() {
                        Ok(p) => open_folder(self.host.get(), &p),
                        Err(e) => error(self.host.get(), e),
                    }
                }
            }
            CMD_FOLDER => open_folder(self.host.get(), &self.store.directory),
            CMD_EXIT => {
                if self.save_all() {
                    DestroyWindow(self.host.get());
                } else {
                    message(self.host.get(),"Some text has not been saved. The app will stay open so you can copy it or retry saving.",MB_OK|MB_ICONWARNING);
                }
            }
            _ => (),
        }
    }
    unsafe fn tray(&self, action: u32) -> bool {
        let mut data: NOTIFYICONDATAW = zeroed();
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = self.host.get();
        data.uID = 1;
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = self.icon;
        let tip = wide("SimpleNotes · click to open the menu");
        data.szTip[..tip.len()].copy_from_slice(&tip);
        Shell_NotifyIconW(action, &data) != 0
    }
    unsafe fn menu(self: &Rc<Self>) {
        let menu = create_dark_menu();
        for (id, label) in [
            (CMD_NEW, "New note"),
            (CMD_LIBRARY, "My notes…"),
            (CMD_SETTINGS, "Settings…"),
        ] {
            append_dark_menu(menu, MF_STRING, id, wide(label).as_ptr());
        }
        append_dark_menu(menu, MF_SEPARATOR, 0, null());
        append_dark_menu(
            menu,
            MF_STRING,
            CMD_HIDE,
            wide(if self.hidden.get() {
                "Show notes"
            } else {
                "Hide notes"
            })
            .as_ptr(),
        );
        let backup = create_dark_menu();
        for (id, label) in [
            (CMD_BACKUP, "Create a backup"),
            (CMD_EXPORT, "Export notes to .txt"),
            (CMD_FOLDER, "Open data folder"),
        ] {
            append_dark_menu(backup, MF_STRING, id, wide(label).as_ptr());
        }
        append_dark_menu(menu, MF_POPUP, backup as usize, wide("Backup").as_ptr());
        append_dark_menu(menu, MF_SEPARATOR, 0, null());
        append_dark_menu(
            menu,
            MF_STRING,
            CMD_EXIT,
            wide("Exit (everything is saved)").as_ptr(),
        );
        let mut pt = zeroed();
        GetCursorPos(&mut pt);
        SetForegroundWindow(self.host.get());
        let id = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            0,
            self.host.get(),
            null(),
        );
        destroy_dark_menu(menu);
        PostMessageW(self.host.get(), WM_NULL, 0, 0);
        self.command(id as usize);
    }
}

unsafe fn work_area(monitor: HMONITOR) -> RECT {
    let mut info: MONITORINFO = zeroed();
    info.cbSize = size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(monitor, &mut info);
    info.rcWork
}
unsafe fn visible_rect(n: &Note) -> RECT {
    let original = RECT {
        left: n.x,
        top: n.y,
        right: n.x.saturating_add(n.width),
        bottom: n.y.saturating_add(n.height),
    };
    let work = work_area(MonitorFromRect(&original, MONITOR_DEFAULTTONEAREST));
    let width = n.width.max(180).min(work.right - work.left);
    let height = n.height.max(120).min(work.bottom - work.top);
    let x = n.x.clamp(work.left, (work.right - width).max(work.left));
    let y = n.y.clamp(work.top, (work.bottom - height).max(work.top));
    RECT {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    }
}

struct NoteWindow {
    app: Rc<App>,
    id: i64,
    edit: Cell<HWND>,
    font: Cell<HFONT>,
    brush: HBRUSH,
    ready: Cell<bool>,
    dirty: Cell<bool>,
    failed: Cell<bool>,
    initial: RefCell<Option<String>>,
    todo: Cell<bool>,
    todo_list: Cell<HWND>,
    todo_input: Cell<HWND>,
    todo_add: Cell<HWND>,
    items: RefCell<Vec<TodoItem>>,
    editing: Cell<bool>,
    updating: Cell<bool>,
    hovered: Cell<bool>,
    chrome_alpha: Cell<u32>,
    rounded: Cell<(i32, i32, i32)>,
}
impl NoteWindow {
    unsafe fn new(app: Rc<App>, note: &Note) -> Box<Self> {
        Box::new(Self {
            app,
            id: note.id,
            edit: Cell::new(null_mut()),
            font: Cell::new(null_mut()),
            brush: CreateSolidBrush(BG),
            ready: Cell::new(false),
            dirty: Cell::new(false),
            failed: Cell::new(false),
            initial: RefCell::new(Some(note.text.clone())),
            todo: Cell::new(note.todo),
            todo_list: Cell::new(null_mut()),
            todo_input: Cell::new(null_mut()),
            todo_add: Cell::new(null_mut()),
            items: RefCell::new(vec![]),
            editing: Cell::new(false),
            updating: Cell::new(false),
            hovered: Cell::new(false),
            chrome_alpha: Cell::new(0),
            rounded: Cell::new((0, 0, 0)),
        })
    }
    unsafe fn px(&self, hwnd: HWND, n: i32) -> i32 {
        (n * GetDpiForWindow(hwnd) as i32 * self.app.scale.get() as i32 / 9600).max(1)
    }
    unsafe fn layout(&self, hwnd: HWND) {
        let r = client(hwnd);
        let pad = self.px(hwnd, 12);
        let top = self.px(hwnd, 32);
        let rounded = (r.right, r.bottom, self.px(hwnd, 14));
        if rounded != self.rounded.get() && r.right > 0 && r.bottom > 0 {
            self.rounded.set(rounded);
            let region = CreateRoundRectRgn(0, 0, r.right + 1, r.bottom + 1, rounded.2, rounded.2);
            if SetWindowRgn(hwnd, region, 1) == 0 {
                DeleteObject(region);
            }
        }
        if self.todo.get() && !self.todo_list.get().is_null() {
            let input_height = self.px(hwnd, 28);
            let width = (r.right - 2 * pad).max(1);
            MoveWindow(
                self.todo_list.get(),
                pad,
                top,
                width,
                (r.bottom - top - input_height - 3 * pad).max(1),
                1,
            );
            MoveWindow(
                self.todo_input.get(),
                pad,
                r.bottom - input_height - pad,
                (width - input_height - pad).max(1),
                input_height,
                1,
            );
            MoveWindow(
                self.todo_add.get(),
                r.right - pad - input_height,
                r.bottom - input_height - pad,
                input_height,
                input_height,
                1,
            );
            SendMessageW(
                self.todo_list.get(),
                LB_SETITEMHEIGHT,
                0,
                self.px(hwnd, 39) as isize,
            );
            return;
        }
        MoveWindow(
            self.edit.get(),
            pad,
            top,
            (r.right - 2 * pad).max(1),
            (r.bottom - top - pad).max(1),
            1,
        );
        let mut edit_rect = client(self.edit.get());
        edit_rect.left = 0;
        edit_rect.top = 0;
        SendMessageW(
            self.edit.get(),
            EM_SETRECT,
            0,
            &edit_rect as *const _ as isize,
        );
    }
    unsafe fn rescale(&self, hwnd: HWND) {
        let new_font = font(self.px(hwnd, 14));
        SendMessageW(self.edit.get(), WM_SETFONT, new_font as usize, 1);
        for child in [
            self.todo_list.get(),
            self.todo_input.get(),
            self.todo_add.get(),
        ] {
            if !child.is_null() {
                SendMessageW(child, WM_SETFONT, new_font as usize, 1);
            }
        }
        let old = self.font.replace(new_font);
        if !old.is_null() {
            DeleteObject(old);
        }
        self.layout(hwnd);
        InvalidateRect(hwnd, null(), 1);
    }
    unsafe fn save(&self, hwnd: HWND) -> bool {
        if !self.ready.get() || !self.dirty.get() {
            return !self.failed.get();
        }
        let result = if self.todo.get() {
            self.app
                .store
                .save_todo_draft(self.id, &text(self.todo_input.get()))
        } else {
            self.app.store.save_text(self.id, &text(self.edit.get()))
        };
        match result {
            Ok(()) => {
                self.dirty.set(false);
                self.failed.set(false);
                KillTimer(hwnd, 2);
                InvalidateRect(hwnd, null(), 0);
                true
            }
            Err(_) => {
                self.failed.set(true);
                SetTimer(hwnd, 2, 3000, None);
                InvalidateRect(hwnd, null(), 0);
                false
            }
        }
    }
    unsafe fn save_position(&self, hwnd: HWND) {
        if !self.ready.get() {
            return;
        }
        let mut r = zeroed();
        GetWindowRect(hwnd, &mut r);
        if let Err(e) =
            self.app
                .store
                .geometry(self.id, r.left, r.top, r.right - r.left, r.bottom - r.top)
        {
            error(hwnd, e);
        }
    }
    unsafe fn archive(&self, hwnd: HWND) {
        if !self.save(hwnd) {
            error(
                hwnd,
                "The text could not be saved. Copy its contents before closing.",
            );
            return;
        }
        match self.app.store.archive(self.id, true) {
            Ok(()) => {
                self.app.refresh_library();
                DestroyWindow(hwnd);
            }
            Err(e) => error(hwnd, e),
        }
    }

    unsafe fn update_hover(&self, hwnd: HWND) {
        let mut point = POINT { x: 0, y: 0 };
        GetCursorPos(&mut point);
        let hit = WindowFromPoint(point);
        let hovered = hit == hwnd || IsChild(hwnd, hit) != 0;
        if hovered != self.hovered.replace(hovered) {
            SetTimer(hwnd, 4, 16, None);
        }
    }

    unsafe fn toggle_todo(&self, hwnd: HWND) {
        if !self.save(hwnd) {
            return;
        }
        if self.todo.get() {
            match self.app.store.disable_todo(self.id) {
                Ok(content) => {
                    self.ready.set(false);
                    self.todo.set(false);
                    for child in [
                        self.todo_list.replace(null_mut()),
                        self.todo_input.replace(null_mut()),
                        self.todo_add.replace(null_mut()),
                    ] {
                        if !child.is_null() {
                            DestroyWindow(child);
                        }
                    }
                    SetWindowTextW(self.edit.get(), wide(&content).as_ptr());
                    ShowWindow(self.edit.get(), SW_SHOW);
                    self.ready.set(true);
                    self.rescale(hwnd);
                    SetFocus(self.edit.get());
                    self.app.refresh_library();
                }
                Err(e) => error(hwnd, e),
            }
        } else {
            match self.app.store.convert_to_todo(self.id) {
                Ok(()) => {
                    self.todo_controls(hwnd);
                    SetFocus(self.todo_input.get());
                    self.app.refresh_library();
                }
                Err(e) => error(hwnd, e),
            }
        }
    }

    unsafe fn todo_controls(&self, hwnd: HWND) {
        ShowWindow(self.edit.get(), SW_HIDE);
        self.todo.set(true);
        if self.todo_list.get().is_null() {
            let list = CreateWindowExW(
                0,
                wide("LISTBOX").as_ptr(),
                wide("Tasks").as_ptr(),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_TABSTOP
                    | WS_VSCROLL
                    | LBS_NOTIFY as u32
                    | LBS_OWNERDRAWFIXED as u32
                    | LBS_HASSTRINGS as u32
                    | LBS_NOINTEGRALHEIGHT as u32,
                0,
                0,
                1,
                1,
                hwnd,
                ID_TODO_LIST as HMENU,
                module(),
                null(),
            );
            let input = CreateWindowExW(
                0,
                wide("EDIT").as_ptr(),
                null(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
                0,
                0,
                1,
                1,
                hwnd,
                ID_TODO_INPUT as HMENU,
                module(),
                null(),
            );
            let add = CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide("Add task").as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32,
                0,
                0,
                1,
                1,
                hwnd,
                ID_TODO_ADD as HMENU,
                module(),
                null(),
            );
            self.todo_list.set(list);
            self.todo_input.set(input);
            self.todo_add.set(add);
            for child in [list, input, add] {
                SetWindowSubclass(child, Some(hover_control_proc), 900, 0);
            }
            SetWindowSubclass(list, Some(todo_control_proc), ID_TODO_LIST, 0);
            SetWindowSubclass(input, Some(todo_control_proc), ID_TODO_INPUT, 0);
            SendMessageW(input, EM_SETLIMITTEXT, 32767, 0);
            SendMessageW(
                input,
                EM_SETCUEBANNER,
                1,
                wide("New task…").as_ptr() as isize,
            );
        }
        self.refresh_todo(hwnd);
        self.rescale(hwnd);
    }

    unsafe fn refresh_todo(&self, hwnd: HWND) {
        match self.app.store.todo_state(self.id) {
            Ok(state) => {
                let selected = SendMessageW(self.todo_list.get(), LB_GETCURSEL, 0, 0);
                let top = SendMessageW(self.todo_list.get(), LB_GETTOPINDEX, 0, 0);
                *self.items.borrow_mut() = state.items.clone();
                SendMessageW(self.todo_list.get(), WM_SETREDRAW, 0, 0);
                SendMessageW(self.todo_list.get(), LB_RESETCONTENT, 0, 0);
                for item in state.items {
                    let label = format!(
                        "{}: {}",
                        ["Pending", "In progress", "Done"][item.status as usize],
                        item.text
                    );
                    SendMessageW(
                        self.todo_list.get(),
                        LB_ADDSTRING,
                        0,
                        wide(&label).as_ptr() as isize,
                    );
                }
                if selected >= 0 {
                    SendMessageW(self.todo_list.get(), LB_SETCURSEL, selected as usize, 0);
                }
                SendMessageW(self.todo_list.get(), LB_SETTOPINDEX, top.max(0) as usize, 0);
                SendMessageW(self.todo_list.get(), WM_SETREDRAW, 1, 0);
                self.editing.set(state.editing.is_some());
                self.updating.set(true);
                SetWindowTextW(self.todo_input.get(), wide(&state.draft).as_ptr());
                SetWindowTextW(
                    self.todo_add.get(),
                    wide(if self.editing.get() {
                        "Save task"
                    } else {
                        "Add task"
                    })
                    .as_ptr(),
                );
                self.updating.set(false);
                InvalidateRect(self.todo_list.get(), null(), 1);
                InvalidateRect(self.todo_add.get(), null(), 1);
                InvalidateRect(hwnd, null(), 0);
                self.app.refresh_library();
            }
            Err(e) => error(hwnd, e),
        }
    }

    unsafe fn todo_action(&self, hwnd: HWND, edit: bool, index: usize) {
        if !self.save(hwnd) {
            error(hwnd, "Could not save the task draft.");
            return;
        }
        let item = self.items.borrow().get(index).cloned();
        let Some(item) = item else {
            return;
        };
        if edit {
            if !text(self.todo_input.get()).trim().is_empty() {
                message(hwnd,"First save the task you are editing by pressing Enter or clicking the button on the right.",MB_OK|MB_ICONINFORMATION);
                return;
            }
            match self.app.store.edit_todo_item(self.id, item.id) {
                Ok(()) => {
                    self.refresh_todo(hwnd);
                    SetFocus(self.todo_input.get());
                    SendMessageW(self.todo_input.get(), EM_SETSEL, 0, -1);
                }
                Err(e) => error(hwnd, e),
            }
        } else {
            match self.app.store.cycle_todo_item(self.id, item.id) {
                Ok(complete) => {
                    self.refresh_todo(hwnd);
                    if complete
                        && message(
                            hwnd,
                            "All tasks are done!\n\nWould you like to archive this list?",
                            MB_YESNO | MB_ICONQUESTION,
                        ) == IDYES
                    {
                        self.archive(hwnd);
                    }
                }
                Err(e) => error(hwnd, e),
            }
        }
    }

    unsafe fn draw_todo(&self, hwnd: HWND, draw: &DRAWITEMSTRUCT) {
        let dc = draw.hDC;
        let r = draw.rcItem;
        let selected = draw.itemState & ODS_SELECTED != 0;
        let brush = CreateSolidBrush(if selected { 0x00424242 } else { BG });
        FillRect(dc, &r, brush);
        DeleteObject(brush);
        let old = SelectObject(dc, self.font.get());
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, FG);
        if draw.CtlID as usize == ID_TODO_ADD {
            let mut label = r;
            DrawTextW(
                dc,
                wide(if self.editing.get() { "✓" } else { "+" }).as_ptr(),
                -1,
                &mut label,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        } else {
            let item = self.items.borrow().get(draw.itemID as usize).cloned();
            if let Some(item) = item {
                let color = [0x005858ef, ACCENT, 0x0082ca65][item.status as usize];
                let brush = CreateSolidBrush(color);
                let oldbrush = SelectObject(dc, brush);
                let oldpen = SelectObject(dc, GetStockObject(NULL_PEN));
                let x = r.left + self.px(hwnd, 5);
                let y = r.top + self.px(hwnd, 9);
                let size = self.px(hwnd, 11);
                Ellipse(dc, x, y, x + size, y + size);
                SelectObject(dc, oldpen);
                SelectObject(dc, oldbrush);
                DeleteObject(brush);
                let mut title = RECT {
                    left: r.left + self.px(hwnd, 25),
                    top: r.top + self.px(hwnd, 2),
                    right: r.right - self.px(hwnd, 4),
                    bottom: r.top + self.px(hwnd, 22),
                };
                DrawTextW(
                    dc,
                    wide(&item.text).as_ptr(),
                    -1,
                    &mut title,
                    DT_SINGLELINE | DT_END_ELLIPSIS | DT_VCENTER,
                );
                let small = font(self.px(hwnd, 11));
                SelectObject(dc, small);
                SetTextColor(dc, 0x00b5b5b5);
                let mut status = RECT {
                    left: title.left,
                    top: r.top + self.px(hwnd, 22),
                    right: title.right,
                    bottom: r.bottom,
                };
                DrawTextW(
                    dc,
                    wide(["Pending", "In progress", "Done"][item.status as usize]).as_ptr(),
                    -1,
                    &mut status,
                    DT_SINGLELINE,
                );
                SelectObject(dc, self.font.get());
                DeleteObject(small);
            }
        }
        if draw.itemState & ODS_FOCUS != 0 {
            DrawFocusRect(dc, &r);
        }
        SelectObject(dc, old);
    }
}

unsafe extern "system" fn hover_control_proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    if msg == WM_CONTEXTMENU {
        return SendMessageW(GetParent(hwnd), msg, hwnd as usize, l);
    }
    if matches!(msg, WM_MOUSEMOVE | WM_MOUSELEAVE) {
        SendMessageW(GetParent(hwnd), WM_CHROME_HOVER, 0, 0);
        if msg == WM_MOUSEMOVE {
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut track);
        }
    }
    if msg == WM_NCDESTROY {
        RemoveWindowSubclass(hwnd, Some(hover_control_proc), id);
    }
    DefSubclassProc(hwnd, msg, w, l)
}

fn fade_color(foreground: u32, background: u32, alpha: u32) -> u32 {
    let mut result = 0;
    for shift in [0, 8, 16] {
        let a = (foreground >> shift) & 255;
        let b = (background >> shift) & 255;
        result |= ((a * alpha + b * (255 - alpha)) / 255) << shift;
    }
    result
}

unsafe extern "system" fn todo_control_proc(
    hwnd: HWND,
    msg: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    let parent = GetParent(hwnd);
    if id == ID_TODO_LIST && msg == WM_LBUTTONDBLCLK {
        let ptr = GetWindowLongPtrW(parent, GWLP_USERDATA) as *const NoteWindow;
        if !ptr.is_null() && (l as i16 as i32) < (*ptr).px(parent, 25) {
            return DefSubclassProc(hwnd, WM_LBUTTONDOWN, w, l);
        }
    }
    if id == ID_TODO_INPUT && msg == WM_KEYDOWN && w == VK_RETURN as usize {
        SendMessageW(parent, WM_COMMAND, ID_TODO_ADD, 0);
        return 0;
    }
    if id == ID_TODO_INPUT && msg == WM_CHAR && w == VK_RETURN as usize {
        return 0;
    }
    if id == ID_TODO_LIST && msg == WM_KEYDOWN && (w == VK_SPACE as usize || w == VK_F2 as usize) {
        let index = SendMessageW(hwnd, LB_GETCURSEL, 0, 0);
        if index >= 0 {
            PostMessageW(
                parent,
                WM_TODO_ACTION,
                (w == VK_F2 as usize) as usize,
                index,
            );
        }
        return 0;
    }
    if msg == WM_NCDESTROY {
        RemoveWindowSubclass(hwnd, Some(todo_control_proc), id);
    }
    let result = DefSubclassProc(hwnd, msg, w, l);
    if id == ID_TODO_LIST && msg == WM_LBUTTONUP {
        let ptr = GetWindowLongPtrW(parent, GWLP_USERDATA) as *const NoteWindow;
        if !ptr.is_null() && (l as i16 as i32) < (*ptr).px(parent, 25) {
            let hit = SendMessageW(hwnd, LB_ITEMFROMPOINT, 0, l);
            if hi(hit as usize) == 0 {
                PostMessageW(parent, WM_TODO_ACTION, 0, lo(hit as usize) as isize);
            }
        }
    }
    result
}

unsafe extern "system" fn note_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = &*(l as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const NoteWindow;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let n = &*ptr;
    match msg {
        WM_NCCALCSIZE => return 0, // Keep native resize behavior with a compact custom frame.
        WM_NCPAINT => return 0,
        WM_NCACTIVATE => return 1,
        WM_CREATE => {
            // WS_THICKFRAME enables sizing, but neither DWM nor DefWindowProc
            // should draw a system frame around this entirely custom client area.
            let policy = DWMNCRP_DISABLED;
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_NCRENDERING_POLICY as u32,
                &policy as *const _ as *const c_void,
                size_of::<i32>() as u32,
            );
            SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
            let edit = CreateWindowExW(
                0,
                wide("EDIT").as_ptr(),
                wide(&n.initial.borrow_mut().take().unwrap_or_default()).as_ptr(),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_TABSTOP
                    | ES_MULTILINE as u32
                    | ES_AUTOVSCROLL as u32
                    | ES_WANTRETURN as u32,
                0,
                0,
                1,
                1,
                hwnd,
                ID_EDIT as HMENU,
                module(),
                null(),
            );
            if edit.is_null() {
                return -1;
            }
            n.edit.set(edit);
            SetWindowSubclass(edit, Some(hover_control_proc), 900, 0);
            SendMessageW(edit, EM_SETLIMITTEXT, 8 * 1024 * 1024, 0);
            n.rescale(hwnd);
            n.ready.set(true);
            if n.todo.get() {
                n.todo_controls(hwnd);
            }
            return 0;
        }
        WM_COMMAND if lo(w) == ID_EDIT && hi(w) == EN_CHANGE => {
            if n.ready.get() && !n.todo.get() {
                n.dirty.set(true);
                n.save(hwnd);
            }
            return 0;
        }
        WM_COMMAND if lo(w) == ID_TODO_INPUT && hi(w) == EN_CHANGE => {
            if n.ready.get() && !n.updating.get() {
                n.dirty.set(true);
                n.save(hwnd);
            }
            return 0;
        }
        WM_COMMAND if lo(w) == ID_TODO_ADD => {
            if n.save(hwnd) {
                match n.app.store.commit_todo_draft(n.id) {
                    Ok(()) => {
                        n.refresh_todo(hwnd);
                        SetFocus(n.todo_input.get());
                    }
                    Err(e) => error(hwnd, e),
                }
            }
            return 0;
        }
        WM_COMMAND if lo(w) == ID_TODO_LIST && hi(w) == LBN_DBLCLK => {
            let index = SendMessageW(n.todo_list.get(), LB_GETCURSEL, 0, 0);
            if index >= 0 {
                PostMessageW(hwnd, WM_TODO_ACTION, 1, index);
            }
            return 0;
        }
        WM_TODO_ACTION => {
            n.todo_action(hwnd, w != 0, l as usize);
            return 0;
        }
        WM_CHROME_HOVER | WM_MOUSEMOVE | WM_MOUSELEAVE | WM_NCMOUSEMOVE | WM_NCMOUSELEAVE => {
            n.update_hover(hwnd);
            if matches!(msg, WM_MOUSEMOVE | WM_NCMOUSEMOVE) {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE
                        | if msg == WM_NCMOUSEMOVE {
                            TME_NONCLIENT
                        } else {
                            0
                        },
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut track);
            }
            return 0;
        }
        WM_MEASUREITEM => {
            if dark_menu_measure(hwnd, l) {
                return 1;
            }
            let measure = &mut *(l as *mut MEASUREITEMSTRUCT);
            if measure.CtlID as usize == ID_TODO_LIST {
                measure.itemHeight = n.px(hwnd, 39) as u32;
                return 1;
            }
        }
        WM_DRAWITEM => {
            if dark_menu_draw(hwnd, l) {
                return 1;
            }
            let draw = &*(l as *const DRAWITEMSTRUCT);
            if matches!(draw.CtlID as usize, ID_TODO_LIST | ID_TODO_ADD) {
                n.draw_todo(hwnd, draw);
                return 1;
            }
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => {
            let dc = w as HDC;
            SetTextColor(dc, FG);
            SetBkColor(dc, BG);
            return n.brush as isize;
        }
        WM_ERASEBKGND => {
            let r = client(hwnd);
            FillRect(w as HDC, &r, n.brush);
            return 1;
        }
        WM_PAINT => {
            let mut ps = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let r = client(hwnd);
            FillRect(dc, &r, n.brush);
            let accent = CreateSolidBrush(if n.failed.get() { 0x005858ff } else { ACCENT });
            let stripe = RECT {
                left: 0,
                top: 0,
                right: r.right,
                bottom: n.px(hwnd, 3),
            };
            FillRect(dc, &stripe, accent);
            DeleteObject(accent);
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(
                dc,
                if n.failed.get() {
                    0x008888ff
                } else {
                    fade_color(0x00bdbdbd, BG, n.chrome_alpha.get())
                },
            );
            let prev = SelectObject(dc, n.font.get());
            let top = n.px(hwnd, 3);
            let h = n.px(hwnd, 22);
            let button = n.px(hwnd, 25);
            let mut left = RECT {
                left: 0,
                top: 0,
                right: button,
                bottom: h,
            };
            let todo_label = if n.todo.get() {
                let items = n.items.borrow();
                format!(
                    "ToDo · {}/{}",
                    items.iter().filter(|i| i.status == 2).count(),
                    items.len()
                )
            } else {
                String::new()
            };
            DrawTextW(
                dc,
                wide("×").as_ptr(),
                -1,
                &mut left,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            let mut label = RECT {
                left: button + 2,
                top,
                right: r.right - button * 2,
                bottom: h,
            };
            DrawTextW(
                dc,
                wide(if n.failed.get() {
                    "Not saved · retrying"
                } else {
                    &todo_label
                })
                .as_ptr(),
                -1,
                &mut label,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
            let mut dots = RECT {
                left: r.right - button,
                top,
                right: r.right,
                bottom: h,
            };
            DrawTextW(
                dc,
                wide("···").as_ptr(),
                -1,
                &mut dots,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            let mut close = RECT {
                left: r.right - button * 2,
                top,
                right: r.right - button,
                bottom: h,
            };
            DrawTextW(
                dc,
                wide("+").as_ptr(),
                -1,
                &mut close,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            SelectObject(dc, prev);
            EndPaint(hwnd, &ps);
            return 0;
        }
        WM_NCHITTEST => {
            let mut pt = POINT {
                x: l as i16 as i32,
                y: (l >> 16) as i16 as i32,
            };
            ScreenToClient(hwnd, &mut pt);
            let r = client(hwnd);
            let edge = n.px(hwnd, 4);
            if pt.y < n.px(hwnd, 24)
                && (pt.x < n.px(hwnd, 25) || pt.x >= r.right - n.px(hwnd, 50))
                && (n.hovered.get() || n.chrome_alpha.get() > 0)
            {
                return HTCLIENT as isize;
            }
            let bottom_edge = pt.y >= r.bottom - edge;
            let left = pt.x < edge;
            let right = pt.x >= r.right - edge;
            return (if pt.y < edge {
                if left {
                    HTTOPLEFT
                } else if right {
                    HTTOPRIGHT
                } else {
                    HTTOP
                }
            } else if bottom_edge {
                if left {
                    HTBOTTOMLEFT
                } else if right {
                    HTBOTTOMRIGHT
                } else {
                    HTBOTTOM
                }
            } else if left {
                HTLEFT
            } else if right {
                HTRIGHT
            } else if pt.y < n.px(hwnd, 23)
                && pt.x > n.px(hwnd, 25)
                && pt.x < r.right - n.px(hwnd, 50)
            {
                HTCAPTION
            } else {
                HTCLIENT
            }) as isize;
        }
        WM_LBUTTONUP => {
            let x = l as i16 as i32;
            let y = (l >> 16) as i16 as i32;
            let r = client(hwnd);
            let b = n.px(hwnd, 25);
            if y < n.px(hwnd, 24) && (n.hovered.get() || n.chrome_alpha.get() > 0) {
                if x < b {
                    n.archive(hwnd);
                } else if x >= r.right - b {
                    note_menu(hwnd, n, null_mut());
                } else if x >= r.right - 2 * b {
                    n.app.create_note("");
                }
            }
            return 0;
        }
        WM_CONTEXTMENU if w as HWND == hwnd || IsChild(hwnd, w as HWND) != 0 => {
            note_menu(
                hwnd,
                n,
                if w as HWND == n.edit.get() || w as HWND == n.todo_input.get() {
                    w as HWND
                } else {
                    null_mut()
                },
            );
            return 0;
        }
        WM_SETFOCUS => {
            SetFocus(if n.todo.get() {
                n.todo_input.get()
            } else {
                n.edit.get()
            });
            return 0;
        }
        WM_SIZE => {
            n.layout(hwnd);
            return 0;
        }
        WM_EXITSIZEMOVE => {
            n.save_position(hwnd);
            return 0;
        }
        WM_GETMINMAXINFO => {
            let m = &mut *(l as *mut MINMAXINFO);
            m.ptMinTrackSize = POINT {
                x: n.px(hwnd, 180),
                y: n.px(hwnd, 120),
            };
            return 0;
        }
        WM_WINDOWPOSCHANGING => {
            let pos = &mut *(l as *mut WINDOWPOS);
            if pos.flags & SWP_NOZORDER == 0 {
                pos.hwndInsertAfter = HWND_BOTTOM;
                pos.flags |= SWP_NOACTIVATE;
            }
            return 0;
        }
        WM_DPICHANGED => {
            let r = &*(l as *const RECT);
            SetWindowPos(
                hwnd,
                HWND_BOTTOM,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOACTIVATE,
            );
            n.rescale(hwnd);
            return 0;
        }
        WM_SCALE => {
            n.rescale(hwnd);
            return 0;
        }
        WM_TIMER if w == 2 => {
            n.save(hwnd);
            return 0;
        }
        WM_TIMER if w == 4 => {
            n.update_hover(hwnd);
            let alpha = n.chrome_alpha.get();
            let next = if n.hovered.get() {
                (alpha + 32).min(255)
            } else {
                alpha.saturating_sub(24)
            };
            n.chrome_alpha.set(next);
            let header = RECT {
                left: 0,
                top: 0,
                right: client(hwnd).right,
                bottom: n.px(hwnd, 24),
            };
            InvalidateRect(hwnd, &header, 0);
            if next == if n.hovered.get() { 255 } else { 0 } {
                KillTimer(hwnd, 4);
            }
            return 0;
        }
        WM_CLOSE => {
            n.archive(hwnd);
            return 0;
        }
        WM_NCDESTROY => {
            KillTimer(hwnd, 2);
            KillTimer(hwnd, 4);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            n.app.windows.borrow_mut().remove(&n.id);
            DeleteObject(n.font.get());
            DeleteObject(n.brush);
            drop(Box::from_raw(ptr as *mut NoteWindow));
        }
        _ => (),
    }
    DefWindowProcW(hwnd, msg, w, l)
}

unsafe fn build_note_menu(n: &NoteWindow, editor: HWND) -> HMENU {
    let menu = create_dark_menu();
    if !editor.is_null() {
        for (id, label) in [
            (CMD_UNDO, "Undo"),
            (CMD_CUT, "Cut"),
            (CMD_COPY, "Copy"),
            (CMD_PASTE, "Paste"),
            (CMD_SELECT_ALL, "Select all"),
        ] {
            append_dark_menu(menu, MF_STRING, id, wide(label).as_ptr());
        }
        append_dark_menu(menu, MF_SEPARATOR, 0, null());
    }
    for (id, label) in [
        (CMD_NEW, "New note"),
        (CMD_SETTINGS, "Settings…"),
        (CMD_ARCHIVE, "Archive (can be restored)"),
    ] {
        append_dark_menu(menu, MF_STRING, id, wide(label).as_ptr());
    }
    if n.todo.get() || text(n.edit.get()).starts_with('-') {
        append_dark_menu(
            menu,
            MF_STRING,
            CMD_TOGGLE_TODO,
            wide(if n.todo.get() {
                "Disable ToDoList"
            } else {
                "Enable ToDoList"
            })
            .as_ptr(),
        );
    }
    menu
}

unsafe fn note_menu(hwnd: HWND, n: &NoteWindow, editor: HWND) {
    let menu = build_note_menu(n, editor);
    let mut pt = zeroed();
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    let id = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        pt.x,
        pt.y,
        0,
        hwnd,
        null(),
    );
    destroy_dark_menu(menu);
    match id as usize {
        CMD_UNDO => {
            SendMessageW(editor, EM_UNDO, 0, 0);
        }
        CMD_CUT => {
            SendMessageW(editor, WM_CUT, 0, 0);
        }
        CMD_COPY => {
            SendMessageW(editor, WM_COPY, 0, 0);
        }
        CMD_PASTE => {
            SendMessageW(editor, WM_PASTE, 0, 0);
        }
        CMD_SELECT_ALL => {
            SendMessageW(editor, EM_SETSEL, 0, -1);
        }
        CMD_TOGGLE_TODO => n.toggle_todo(hwnd),
        CMD_ARCHIVE => n.archive(hwnd),

        id => n.app.command(id),
    }
}

enum PanelKind {
    Settings,
    Library,
}
struct Panel {
    app: Rc<App>,
    kind: PanelKind,
    font: Cell<HFONT>,
    brush: HBRUSH,
    notes: RefCell<Vec<Note>>,
}
impl Panel {
    unsafe fn px(&self, hwnd: HWND, n: i32) -> i32 {
        n * GetDpiForWindow(hwnd) as i32 / 96
    }
    #[allow(clippy::too_many_arguments)]
    unsafe fn control(
        &self,
        hwnd: HWND,
        class: &str,
        label: &str,
        style: u32,
        id: usize,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> HWND {
        let child = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(label).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            self.px(hwnd, x),
            self.px(hwnd, y),
            self.px(hwnd, width),
            self.px(hwnd, height),
            hwnd,
            id as HMENU,
            module(),
            null(),
        );
        SendMessageW(child, WM_SETFONT, self.font.get() as usize, 1);
        if class == "BUTTON" || id == ID_SCALE {
            SetWindowSubclass(child, Some(dark_control_proc), id, 0);
        }
        if class == "LISTBOX" {
            SetWindowTheme(child, wide("DarkMode_Explorer").as_ptr(), null());
        }
        child
    }
    unsafe fn label(&self, hwnd: HWND, label: &str, x: i32, y: i32, width: i32, height: i32) {
        self.control(hwnd, "STATIC", label, 0, 0, x, y, width, height);
    }
    unsafe fn button(&self, hwnd: HWND, label: &str, id: usize, x: i32, y: i32, width: i32) {
        self.control(hwnd, "BUTTON", label, WS_TABSTOP, id, x, y, width, 32);
    }
    unsafe fn populate(&self, hwnd: HWND) {
        let list = GetDlgItem(hwnd, ID_LIST as i32);
        SendMessageW(list, LB_RESETCONTENT, 0, 0);
        match self.app.store.notes() {
            Ok(notes) => {
                for n in &notes {
                    let preview = n
                        .text
                        .lines()
                        .find(|s| !s.trim().is_empty())
                        .unwrap_or("Empty note")
                        .chars()
                        .take(58)
                        .collect::<String>();
                    let label = format!(
                        "{}  #{}  {}",
                        if n.archived {
                            "[Archived]"
                        } else {
                            "[Desktop]"
                        },
                        n.id,
                        preview
                    );
                    SendMessageW(list, LB_ADDSTRING, 0, wide(&label).as_ptr() as isize);
                }
                *self.notes.borrow_mut() = notes;
                SendMessageW(list, LB_SETCURSEL, 0, 0);
            }
            Err(e) => error(hwnd, e),
        }
    }
    unsafe fn selected(&self, hwnd: HWND) -> Option<Note> {
        let index = SendMessageW(GetDlgItem(hwnd, ID_LIST as i32), LB_GETCURSEL, 0, 0);
        if index < 0 {
            None
        } else {
            self.notes.borrow().get(index as usize).cloned()
        }
    }
    unsafe fn init(&self, hwnd: HWND) {
        self.font.set(font(self.px(hwnd, 15)));
        match self.kind {
            PanelKind::Settings => {
                self.label(hwnd, "Settings", 24, 22, 310, 28);
                let startup = self.control(
                    hwnd,
                    "BUTTON",
                    "Start with Windows",
                    WS_TABSTOP | BS_AUTOCHECKBOX as u32,
                    ID_STARTUP,
                    24,
                    68,
                    310,
                    28,
                );
                SendMessageW(
                    startup,
                    BM_SETCHECK,
                    if startup_enabled() {
                        BST_CHECKED
                    } else {
                        BST_UNCHECKED
                    } as usize,
                    0,
                );
                self.label(hwnd, "Scale", 24, 116, 310, 26);
                let slider = self.control(
                    hwnd,
                    "msctls_trackbar32",
                    "Scale",
                    WS_TABSTOP | TBS_NOTICKS,
                    ID_SCALE,
                    24,
                    146,
                    235,
                    32,
                );
                SendMessageW(slider, TBM_SETRANGEMIN, 0, SCALE_MIN as isize);
                SendMessageW(slider, TBM_SETRANGEMAX, 0, SCALE_MAX as isize);
                SendMessageW(slider, TBM_SETPOS, 1, self.app.scale.get() as isize);
                self.control(
                    hwnd,
                    "STATIC",
                    &format!("{} %", self.app.scale.get()),
                    0,
                    ID_SCALE_VALUE,
                    272,
                    150,
                    64,
                    28,
                );
                self.label(
                    hwnd,
                    "Changes take effect immediately.",
                    24,
                    203,
                    310,
                    24,
                );
            }
            PanelKind::Library => {
                self.label(hwnd, "MY NOTES", 24, 20, 460, 28);
                self.label(
                    hwnd,
                    "Closing a note archives it. You can restore it here.",
                    24,
                    55,
                    470,
                    28,
                );
                self.control(
                    hwnd,
                    "LISTBOX",
                    "Notes",
                    WS_TABSTOP
                        | WS_BORDER
                        | WS_VSCROLL
                        | LBS_NOTIFY as u32
                        | LBS_NOINTEGRALHEIGHT as u32,
                    ID_LIST,
                    24,
                    95,
                    464,
                    292,
                );
                self.button(hwnd, "Open / restore", ID_OPEN, 24, 409, 150);

                self.button(hwnd, "New note", CMD_NEW, 338, 409, 150);
                self.label(
                    hwnd,
                    "Restored notes appear on your desktop.",
                    24,
                    462,
                    470,
                    26,
                );
                self.populate(hwnd);
            }
        }
    }
}

unsafe fn show_panel(app: Rc<App>, kind: PanelKind) {
    let existing = match kind {
        PanelKind::Settings => app.settings.get(),
        PanelKind::Library => app.library.get(),
    };
    if !existing.is_null() {
        ShowWindow(existing, SW_RESTORE);
        SetForegroundWindow(existing);
        return;
    }
    let title = match kind {
        PanelKind::Settings => "Settings · SimpleNotes",
        PanelKind::Library => "My notes · SimpleNotes",
    };
    let dpi = GetDpiForSystem();
    let scale = |n: i32| n * dpi as i32 / 96;
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let compact = matches!(kind, PanelKind::Settings);
    let ex_style = if app.inspect_windows {
        WS_EX_APPWINDOW
    } else {
        WS_EX_TOOLWINDOW
    };
    let mut r = RECT {
        left: 0,
        top: 0,
        right: scale(if compact { 360 } else { 512 }),
        bottom: scale(if compact { 248 } else { 510 }),
    };
    AdjustWindowRectExForDpi(&mut r, style, 0, ex_style, dpi);
    let data = Box::new(Panel {
        app: app.clone(),
        kind,
        font: Cell::new(null_mut()),
        brush: CreateSolidBrush(PANEL_BG),
        notes: RefCell::new(vec![]),
    });
    let hwnd = CreateWindowExW(
        ex_style,
        wide(PANEL_CLASS).as_ptr(),
        wide(title).as_ptr(),
        style,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        r.right - r.left,
        r.bottom - r.top,
        null_mut(),
        null_mut(),
        module(),
        Box::into_raw(data) as *const c_void,
    );
    if hwnd.is_null() {
        error(app.host.get(), "Could not open the panel.");
        return;
    }
    ShowWindow(hwnd, SW_SHOW);
    SetForegroundWindow(hwnd);
}

unsafe extern "system" fn panel_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
        );
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Panel;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let p = &*ptr;
    match msg {
        WM_ERASEBKGND => {
            FillRect(w as HDC, &client(hwnd), p.brush);
            return 1;
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLORLISTBOX | WM_CTLCOLOREDIT => {
            SetTextColor(w as HDC, FG);
            SetBkColor(w as HDC, PANEL_BG);
            return p.brush as isize;
        }
        WM_CREATE => {
            dark_title_bar(hwnd);
            match p.kind {
                PanelKind::Settings => p.app.settings.set(hwnd),
                PanelKind::Library => p.app.library.set(hwnd),
            };
            p.init(hwnd);
            return 0;
        }
        WM_COMMAND => {
            let id = lo(w);
            match id {
                ID_STARTUP if hi(w) == BN_CLICKED => {
                    let enabled =
                        SendMessageW(l as HWND, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                    if let Err(e) = set_startup(enabled) {
                        SendMessageW(
                            l as HWND,
                            BM_SETCHECK,
                            if startup_enabled() {
                                BST_CHECKED
                            } else {
                                BST_UNCHECKED
                            } as usize,
                            0,
                        );
                        error(hwnd, e);
                    }
                }
                ID_OPEN => {
                    if let Some(note) = p.selected(hwnd) {
                        match p.app.store.archive(note.id, false) {
                            Ok(()) => {
                                p.app.hidden.set(false);
                                p.app.show_note(note);
                                p.populate(hwnd);
                            }
                            Err(e) => error(hwnd, e),
                        }
                    }
                }
                _ => p.app.command(id),
            }
            return 0;
        }
        WM_HSCROLL if l as HWND == GetDlgItem(hwnd, ID_SCALE as i32) => {
            let scale = SendMessageW(l as HWND, TBM_GETPOS, 0, 0) as u32;
            SetWindowTextW(
                GetDlgItem(hwnd, ID_SCALE_VALUE as i32),
                wide(&format!("{scale} %")).as_ptr(),
            );
            InvalidateRect(l as HWND, null(), 0);
            if scale != p.app.scale.get() {
                match p.app.store.set_setting("scale", &scale.to_string()) {
                    Ok(()) => {
                        p.app.scale.set(scale);
                        for note in p.app.handles() {
                            SendMessageW(note, WM_SCALE, 0, 0);
                        }
                    }
                    Err(e) => error(hwnd, e),
                }
            }
            return 0;
        }
        m if m == WM_APP + 10 => {
            p.populate(hwnd);
            return 0;
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            return 0;
        }
        WM_NCDESTROY => {
            match p.kind {
                PanelKind::Settings => p.app.settings.set(null_mut()),
                PanelKind::Library => p.app.library.set(null_mut()),
            }
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DeleteObject(p.font.get());
            DeleteObject(p.brush);
            drop(Box::from_raw(ptr as *mut Panel));
        }
        _ => (),
    }
    DefWindowProcW(hwnd, msg, w, l)
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
unsafe fn startup_enabled() -> bool {
    let mut key: HKEY = null_mut();
    if RegOpenKeyExW(
        HKEY_CURRENT_USER,
        wide(RUN_KEY).as_ptr(),
        0,
        KEY_QUERY_VALUE,
        &mut key,
    ) != ERROR_SUCCESS
    {
        return false;
    }
    let result = RegQueryValueExW(
        key,
        wide("SimpleNotes").as_ptr(),
        null(),
        null_mut(),
        null_mut(),
        null_mut(),
    );
    RegCloseKey(key);
    result == ERROR_SUCCESS
}
unsafe fn set_startup(enabled: bool) -> crate::storage::Result<()> {
    let mut key: HKEY = null_mut();
    let code = RegCreateKeyExW(
        HKEY_CURRENT_USER,
        wide(RUN_KEY).as_ptr(),
        0,
        null(),
        REG_OPTION_NON_VOLATILE,
        KEY_SET_VALUE,
        null(),
        &mut key,
        null_mut(),
    );
    if code != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(code as i32).into());
    }
    let result = if enabled {
        let path = std::env::current_exe()?;
        let command = wide(&format!("\"{}\" --autostart", path.display()));
        RegSetValueExW(
            key,
            wide("SimpleNotes").as_ptr(),
            0,
            REG_SZ,
            command.as_ptr() as *const u8,
            (command.len() * 2) as u32,
        )
    } else {
        RegDeleteValueW(key, wide("SimpleNotes").as_ptr())
    };
    RegCloseKey(key);
    if result != ERROR_SUCCESS && result != ERROR_FILE_NOT_FOUND {
        return Err(std::io::Error::from_raw_os_error(result as i32).into());
    }
    Ok(())
}

unsafe extern "system" fn foreground_event(
    _: HWINEVENTHOOK,
    _: u32,
    _: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    HOST.with(|h| {
        if !h.get().is_null() {
            PostMessageW(h.get(), WM_LOWER, 0, 0);
        }
    });
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        SetWindowLongPtrW(
            hwnd,
            GWLP_USERDATA,
            (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
        );
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Rc<App>;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, w, l);
    }
    let app = &*ptr;
    if msg == app.taskbar_message {
        app.tray(NIM_ADD);
        return 0;
    }
    match msg {
        WM_MEASUREITEM if dark_menu_measure(hwnd, l) => return 1,
        WM_DRAWITEM if dark_menu_draw(hwnd, l) => return 1,
        WM_TRAY => {
            match l as u32 {
                WM_RBUTTONUP | WM_CONTEXTMENU | WM_LBUTTONUP => app.menu(),
                _ => (),
            };
            return 0;
        }
        WM_COMMAND => {
            app.command(lo(w));
            return 0;
        }
        WM_LOWER => {
            app.lower();
            return 0;
        }
        WM_DISPLAYCHANGE => {
            for window in app.handles() {
                let mut r = zeroed();
                GetWindowRect(window, &mut r);
                let note = Note {
                    id: 0,
                    text: String::new(),
                    x: r.left,
                    y: r.top,
                    width: r.right - r.left,
                    height: r.bottom - r.top,
                    archived: false,
                    todo: false,
                };
                let r = visible_rect(&note);
                SetWindowPos(
                    window,
                    HWND_BOTTOM,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOACTIVATE,
                );
            }
            return 0;
        }
        WM_TIMER => {
            if let Err(e) = app.store.backup(false) {
                eprintln!("Automatic backup: {e}");
            }
            return 0;
        }
        WM_QUERYENDSESSION => {
            return app.save_all() as isize;
        }
        WM_ENDSESSION if w != 0 => {
            app.save_all();
            let _ = app.store.checkpoint();
            return 0;
        }
        WM_CLOSE => {
            app.command(CMD_EXIT);
            return 0;
        }
        WM_DESTROY => {
            app.tray(NIM_DELETE);
            KillTimer(hwnd, 1);
            let _ = app.store.checkpoint();
            PostQuitMessage(0);
            return 0;
        }
        WM_NCDESTROY => {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Box::from_raw(ptr as *mut Rc<App>));
        }
        _ => (),
    }
    DefWindowProcW(hwnd, msg, w, l)
}

unsafe fn make_icon() -> HICON {
    // Small opaque DIB icon: amber paper on charcoal, drawn with GDI once.
    let dc = GetDC(null_mut());
    let mem = CreateCompatibleDC(dc);
    let bitmap = CreateCompatibleBitmap(dc, 32, 32);
    let old = SelectObject(mem, bitmap);
    let dark = CreateSolidBrush(BG);
    let amber = CreateSolidBrush(ACCENT);
    let ink = CreateSolidBrush(0x00504c40);
    FillRect(
        mem,
        &RECT {
            left: 0,
            top: 0,
            right: 32,
            bottom: 32,
        },
        dark,
    );
    FillRect(
        mem,
        &RECT {
            left: 4,
            top: 3,
            right: 28,
            bottom: 29,
        },
        amber,
    );
    for y in [11, 16, 21] {
        FillRect(
            mem,
            &RECT {
                left: 9,
                top: y,
                right: 23,
                bottom: y + 2,
            },
            ink,
        );
    }
    SelectObject(mem, old);
    DeleteObject(dark);
    DeleteObject(amber);
    DeleteObject(ink);
    DeleteDC(mem);
    ReleaseDC(null_mut(), dc);
    let bits = [0u8; 128];
    let mask = CreateBitmap(32, 32, 1, 1, bits.as_ptr() as *const c_void);
    let info = ICONINFO {
        fIcon: 1,
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: bitmap,
    };
    let icon = CreateIconIndirect(&info);
    DeleteObject(mask);
    DeleteObject(bitmap);
    icon
}

unsafe fn focus_existing_instance(silent: bool) -> bool {
    let other = FindWindowW(wide(HOST_CLASS).as_ptr(), null());
    if other.is_null() {
        return false;
    }
    if !silent {
        let mut pid = 0;
        GetWindowThreadProcessId(other, &mut pid);
        AllowSetForegroundWindow(pid);
        PostMessageW(other, WM_COMMAND, CMD_SETTINGS, 0);
    }
    true
}

pub fn run() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let controls = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_BAR_CLASSES,
        };
        InitCommonControlsEx(&controls);
        let args = std::env::args_os().collect::<Vec<_>>();
        let directory = if let Some(index) = args.iter().position(|s| s == "--data-dir") {
            match args.get(index + 1) {
                Some(p) => PathBuf::from(p),
                None => {
                    error(null_mut(), "A path is required after --data-dir.");
                    return;
                }
            }
        } else {
            match std::env::var_os("LOCALAPPDATA") {
                Some(p) => PathBuf::from(p).join("SimpleNotes"),
                None => {
                    error(null_mut(), "LOCALAPPDATA could not be found.");
                    return;
                }
            }
        };
        // Also recognize previous releases that used directory-specific locks.
        if focus_existing_instance(args.iter().any(|s| s == "--autostart")) {
            return;
        }
        let _instance = match crate::instance::Instance::acquire(crate::instance::NAME) {
            Ok(Some(instance)) => instance,
            Ok(None) => {
                focus_existing_instance(args.iter().any(|s| s == "--autostart"));
                return;
            }
            Err(e) => {
                error(null_mut(), e);
                return;
            }
        };
        let store = match Store::open(&directory) {
            Ok(s) => Rc::new(s),
            Err(e) => {
                error(null_mut(),format!("{e}\n\nData: {}\nThe database was not replaced. Backups are in the backups subfolder.",directory.display()));
                return;
            }
        };
        let scale = store
            .setting("scale")
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .filter(|s| (SCALE_MIN..=SCALE_MAX).contains(s))
            .unwrap_or(100);
        let icon = make_icon();
        let classes: [(&str, WNDPROC, i32); 3] = [
            (HOST_CLASS, Some(host_proc), 0),
            (NOTE_CLASS, Some(note_proc), 0),
            (PANEL_CLASS, Some(panel_proc), COLOR_WINDOW + 1),
        ];
        for (name, proc, background) in classes {
            let mut class: WNDCLASSEXW = zeroed();
            class.cbSize = size_of::<WNDCLASSEXW>() as u32;
            class.lpfnWndProc = proc;
            class.hInstance = module();
            class.hCursor = LoadCursorW(null_mut(), IDC_ARROW);
            class.hbrBackground = background as HBRUSH;
            class.hIcon = icon;
            class.hIconSm = icon;
            let name = wide(name);
            class.lpszClassName = name.as_ptr();
            if RegisterClassExW(&class) == 0 {
                error(null_mut(), std::io::Error::last_os_error());
                return;
            }
        }
        let app = Rc::new(App {
            store,
            host: Cell::new(null_mut()),
            windows: RefCell::new(HashMap::new()),
            settings: Cell::new(null_mut()),
            library: Cell::new(null_mut()),
            scale: Cell::new(scale),
            hidden: Cell::new(false),
            icon,
            taskbar_message: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            inspect_windows: args.iter().any(|s| s == "--inspect-windows"),
        });
        let host = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            wide(HOST_CLASS).as_ptr(),
            wide(crate::instance::NAME).as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            module(),
            Box::into_raw(Box::new(app.clone())) as *const c_void,
        );
        if host.is_null() {
            error(null_mut(), std::io::Error::last_os_error());
            return;
        }
        app.host.set(host);
        HOST.with(|h| h.set(host));
        if !app.tray(NIM_ADD) {
            show_panel(app.clone(), PanelKind::Settings);
            error(host,"Could not add the system tray icon. Reopen SimpleNotes once your desktop has finished loading.");
        }
        match app.store.notes() {
            Ok(notes) => {
                if notes.is_empty() {
                    app.create_note("");
                } else {
                    for note in notes.into_iter().filter(|n| !n.archived) {
                        app.show_note(note);
                    }
                }
            }
            Err(e) => error(host, e),
        }
        if let Err(e) = app.store.backup(false) {
            error(
                host,
                format!("Your notes are being saved, but the backup failed: {e}"),
            );
        }
        SetTimer(host, 1, 60 * 60 * 1000, None);
        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            null_mut(),
            Some(foreground_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
        if args.iter().any(|s| s == "--settings") {
            show_panel(app.clone(), PanelKind::Settings);
        }
        let mut msg: MSG = zeroed();
        loop {
            let result = GetMessageW(&mut msg, null_mut(), 0, 0);
            if result <= 0 {
                break;
            }
            let root = GetAncestor(msg.hwnd, GA_ROOT);
            if msg.message == WM_KEYDOWN && GetKeyState(VK_CONTROL as i32) < 0 {
                match msg.wParam as u16 {
                    0x4e => {
                        app.command(CMD_NEW);
                        continue;
                    } // Ctrl+N
                    0xbc => {
                        app.command(CMD_SETTINGS);
                        continue;
                    } // Ctrl+,
                    0x53 => {
                        app.save_all();
                        continue;
                    } // Ctrl+S
                    0x41 => {
                        // Native multiline EDIT needs an explicit select-all shortcut.
                        let mut child_class = [0u16; 32];
                        GetClassNameW(msg.hwnd, child_class.as_mut_ptr(), 32);
                        if String::from_utf16_lossy(&child_class).trim_end_matches('\0') == "Edit" {
                            SendMessageW(msg.hwnd, EM_SETSEL, 0, -1);
                            continue;
                        }
                    }
                    _ => (),
                }
            }
            let mut class = [0u16; 64];
            GetClassNameW(root, class.as_mut_ptr(), 64);
            let class_name = String::from_utf16_lossy(&class);
            let class_name = class_name.trim_end_matches('\0');
            if class_name == NOTE_CLASS
                && msg.message == WM_KEYDOWN
                && msg.wParam == VK_TAB as usize
            {
                let ptr = GetWindowLongPtrW(root, GWLP_USERDATA) as *const NoteWindow;
                if !ptr.is_null() && (*ptr).todo.get() {
                    let controls = [
                        (*ptr).todo_list.get(),
                        (*ptr).todo_input.get(),
                        (*ptr).todo_add.get(),
                    ];
                    let current = controls.iter().position(|h| *h == GetFocus()).unwrap_or(0);
                    let next = (current
                        + if GetKeyState(VK_SHIFT as i32) < 0 {
                            2
                        } else {
                            1
                        })
                        % 3;
                    SetFocus(controls[next]);
                    continue;
                }
            }
            let panel = class_name == PANEL_CLASS;
            if panel && IsDialogMessageW(root, &msg) != 0 {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if !hook.is_null() {
            UnhookWinEvent(hook);
        }
        HOST.with(|h| h.set(null_mut()));
        for window in app.handles() {
            DestroyWindow(window);
        }
        DestroyIcon(icon);
        MENU_BRUSH.with(|b| {
            let brush = b.replace(null_mut());
            if !brush.is_null() {
                DeleteObject(brush);
            }
        });
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;

    #[test]
    fn hidden_native_widgets_support_dark_theme_slider_rounding_and_manual_todo() {
        unsafe {
            let directory =
                std::env::temp_dir().join(format!("simplenotes-native-{}", std::process::id()));
            let app = Rc::new(App {
                store: Rc::new(Store::open(&directory).unwrap()),
                host: Cell::new(null_mut()),
                windows: RefCell::new(HashMap::new()),
                settings: Cell::new(null_mut()),
                library: Cell::new(null_mut()),
                scale: Cell::new(100),
                hidden: Cell::new(true),
                icon: null_mut(),
                taskbar_message: 0,
                inspect_windows: false,
            });
            let common = INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_BAR_CLASSES,
            };
            InitCommonControlsEx(&common);
            let classes: [(&str, WNDPROC); 2] = [
                (NOTE_CLASS, Some(note_proc)),
                (PANEL_CLASS, Some(panel_proc)),
            ];
            for (name, proc) in classes {
                let mut wc: WNDCLASSEXW = zeroed();
                wc.cbSize = size_of::<WNDCLASSEXW>() as u32;
                wc.lpfnWndProc = proc;
                wc.hInstance = module();
                let name = wide(name);
                wc.lpszClassName = name.as_ptr();
                assert_ne!(RegisterClassExW(&wc), 0);
            }
            let initial = app.store.create("-dsadsa\n-dasdsadsa\n-", 10, 10).unwrap();
            let note = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                wide(NOTE_CLASS).as_ptr(),
                wide("Hidden UI test").as_ptr(),
                WS_POPUP | WS_THICKFRAME | WS_CLIPCHILDREN,
                10,
                10,
                280,
                260,
                null_mut(),
                null_mut(),
                module(),
                Box::into_raw(NoteWindow::new(app.clone(), &initial)) as *const c_void,
            );
            assert!(!note.is_null());
            assert_eq!(IsWindowVisible(note), 0);
            let n = &*(GetWindowLongPtrW(note, GWLP_USERDATA) as *const NoteWindow);
            assert_eq!(n.chrome_alpha.get(), 0);
            assert!(!n.todo.get());
            let region = CreateRectRgn(0, 0, 0, 0);
            assert_ne!(GetWindowRgn(note, region), ERROR);
            assert_eq!(PtInRegion(region, 0, 0), 0);
            assert_ne!(PtInRegion(region, 20, 20), 0);
            DeleteObject(region);
            let menu = build_note_menu(n, null_mut());
            assert_ne!(
                GetMenuState(menu, CMD_TOGGLE_TODO as u32, MF_BYCOMMAND),
                u32::MAX
            );
            destroy_dark_menu(menu);
            n.toggle_todo(note);
            assert!(n.todo.get());
            assert_eq!(n.items.borrow().len(), 2);
            n.toggle_todo(note);
            assert!(!n.todo.get());
            assert_eq!(text(n.edit.get()), "- dsadsa\r\n- dasdsadsa");
            let panel = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                wide(PANEL_CLASS).as_ptr(),
                wide("Hidden settings test").as_ptr(),
                WS_OVERLAPPED | WS_CAPTION,
                20,
                20,
                380,
                290,
                null_mut(),
                null_mut(),
                module(),
                Box::into_raw(Box::new(Panel {
                    app: app.clone(),
                    kind: PanelKind::Settings,
                    font: Cell::new(null_mut()),
                    brush: CreateSolidBrush(PANEL_BG),
                    notes: RefCell::new(vec![]),
                })) as *const c_void,
            );
            assert!(!panel.is_null());
            assert_eq!(IsWindowVisible(panel), 0);
            let slider = GetDlgItem(panel, ID_SCALE as i32);
            assert!(!slider.is_null());
            SendMessageW(slider, TBM_SETPOS, 1, 137);
            SendMessageW(panel, WM_HSCROLL, TB_THUMBTRACK as usize, slider as isize);
            assert_eq!(app.scale.get(), 137);
            assert_eq!(app.store.setting("scale").unwrap().as_deref(), Some("137"));
            assert_eq!(text(GetDlgItem(panel, ID_SCALE_VALUE as i32)), "137 %");
            let screen = GetDC(null_mut());
            let dc = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, 400, 300);
            let old = SelectObject(dc, bitmap);
            let white = GetStockObject(WHITE_BRUSH);
            FillRect(
                dc,
                &RECT {
                    left: 0,
                    top: 0,
                    right: 400,
                    bottom: 300,
                },
                white as HBRUSH,
            );
            SendMessageW(slider, WM_PRINTCLIENT, dc as usize, 0);
            assert_eq!(GetPixel(dc, 0, 0), PANEL_BG);
            let mut menu_item: MENUITEMINFOW = zeroed();
            menu_item.cbSize = size_of::<MENUITEMINFOW>() as u32;
            menu_item.fMask = MIIM_DATA;
            let menu = build_note_menu(n, null_mut());
            assert_ne!(GetMenuItemInfoW(menu, 0, 1, &mut menu_item), 0);
            let draw = DRAWITEMSTRUCT {
                CtlType: ODT_MENU,
                CtlID: 0,
                itemID: CMD_NEW as u32,
                itemAction: ODA_DRAWENTIRE,
                itemState: 0,
                hwndItem: menu as HWND,
                hDC: dc,
                rcItem: RECT {
                    left: 0,
                    top: 0,
                    right: 280,
                    bottom: 30,
                },
                itemData: menu_item.dwItemData,
            };
            assert!(dark_menu_draw(panel, &draw as *const _ as isize));
            assert_eq!(GetPixel(dc, 0, 0), PANEL_BG);
            destroy_dark_menu(menu);
            SelectObject(dc, old);
            DeleteObject(bitmap);
            DeleteDC(dc);
            ReleaseDC(null_mut(), screen);
            DestroyWindow(panel);
            DestroyWindow(note);
            drop(app);
            std::fs::remove_dir_all(directory).unwrap();
            MENU_BRUSH.with(|b| {
                let brush = b.replace(null_mut());
                if !brush.is_null() {
                    DeleteObject(brush);
                }
            });
            assert_eq!(fade_color(FG, BG, 0), BG);
            assert_eq!(fade_color(FG, BG, 255), FG);
        }
    }
}
