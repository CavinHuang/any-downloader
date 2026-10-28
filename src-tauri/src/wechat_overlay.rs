//! 微信视频窗口右上角悬浮下载图标的窗口跟踪（仅 Windows）。
//!
//! 轮询 EnumWindows 找视频号宿主窗口（进程 WeChatAppEx.exe / Weixin.exe /
//! WeChat.exe——视频号由微信内置 Chromium 进程渲染，独立顶层窗），把
//! `sniff-overlay` 窗口钉在其右上角（right-52, top+12，物理像素）。窗口挑选
//! 抽成纯函数便于单测；无候选即隐藏悬浮窗。

#![cfg(windows)]

/// 视频号宿主进程名单（小写文件名）。命中其一即视为候选。
pub const WECHAT_PROCS: &[&str] = &["wechatappex.exe", "weixin.exe", "wechat.exe"];

pub const MIN_W: i32 = 320;
pub const MIN_H: i32 = 320;

#[derive(Debug, Clone, PartialEq)]
pub struct WinInfo {
    pub hwnd: isize,
    pub process: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// 纯函数：从候选窗中挑悬浮宿主——含光标者优先（视频号浮层通常在指针附近），
/// 其次面积最大（主播放窗）；尺寸不足/无效排除。
pub fn pick_window(wins: &[WinInfo], cursor: Option<(i32, i32)>) -> Option<WinInfo> {
    let ok: Vec<&WinInfo> = wins
        .iter()
        .filter(|w| w.w >= MIN_W && w.h >= MIN_H)
        .collect();
    if ok.is_empty() {
        return None;
    }
    // 光标到窗口矩形的距离（在内=0，在外=到最近边）；平手取面积更小者
    // （嵌套时内层=视频号浮层优先，与浏览器浮标同语义）
    let rect_dist2 = |w: &WinInfo, (cx, cy): (i32, i32)| -> i64 {
        let dx = if cx >= w.x && cx <= w.x + w.w {
            0
        } else {
            ((w.x - cx).abs()).min(((w.x + w.w) - cx).abs())
        };
        let dy = if cy >= w.y && cy <= w.y + w.h {
            0
        } else {
            ((w.y - cy).abs()).min(((w.y + w.h) - cy).abs())
        };
        (dx as i64) * (dx as i64) + (dy as i64) * (dy as i64)
    };
    match cursor {
        Some(c) => ok
            .into_iter()
            .min_by_key(|w| (rect_dist2(w, c), (w.w as i64) * (w.h as i64)))
            .cloned(),
        None => ok
            .into_iter()
            .max_by_key(|w| (w.w as i64) * (w.h as i64))
            .cloned(),
    }
}

struct EnumCtx(Vec<WinInfo>);

/// 当前全部候选窗（物理像素；EnumWindows 自顶层 Z 序返回）。
pub fn wechat_windows() -> Vec<WinInfo> {
    let mut ctx = EnumCtx(Vec::new());
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(enum_proc),
            &mut ctx as *mut EnumCtx as isize,
        );
    }
    ctx.0
}

unsafe extern "system" fn enum_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::BOOL {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
    };
    let ctx = &mut *(lparam as *mut EnumCtx);
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    let Some(name) = process_name(pid) else {
        return 1;
    };
    if !WECHAT_PROCS.contains(&name.as_str()) {
        return 1;
    }
    let mut r = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    if GetWindowRect(hwnd, &mut r) == 0 {
        return 1;
    }
    ctx.0.push(WinInfo {
        hwnd: hwnd as isize,
        process: name,
        x: r.left,
        y: r.top,
        w: r.right - r.left,
        h: r.bottom - r.top,
    });
    1
}

/// 进程主模块文件名（小写）；失败返回 None（权限/已退出）。
fn process_name(pid: u32) -> Option<String> {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        windows_sys::Win32::Foundation::CloseHandle(h);
        if ok == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit(['\\', '/'])
            .next()
            .map(|s| s.to_ascii_lowercase())
    }
}

/// 当前光标位置（屏幕物理像素）。
pub fn cursor_pos() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut p) } != 0 {
        Some((p.x, p.y))
    } else {
        None
    }
}

/// 单调递增的窗口 Z 序序号（测试桩无需真实 HWND）。
#[cfg(test)]
pub fn fake_hwnd() -> isize {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed) as isize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(x: i32, y: i32, ww: i32, hh: i32, proc: &str) -> WinInfo {
        WinInfo {
            hwnd: fake_hwnd(),
            process: proc.into(),
            x,
            y,
            w: ww,
            h: hh,
        }
    }

    #[test]
    fn picks_cursor_containing_window() {
        let wins = vec![
            w(0, 0, 1920, 1080, "weixin.exe"),
            w(400, 200, 700, 900, "wechatappex.exe"), // 视频号浮层，含光标
        ];
        let pick = pick_window(&wins, Some((700, 500))).unwrap();
        assert_eq!(pick.process, "wechatappex.exe");
    }

    #[test]
    fn falls_back_to_largest_without_cursor() {
        let wins = vec![
            w(100, 100, 400, 400, "wechatappex.exe"),
            w(0, 0, 1200, 800, "weixin.exe"),
        ];
        let pick = pick_window(&wins, None).unwrap();
        assert_eq!(pick.process, "weixin.exe");
    }

    #[test]
    fn skips_tiny_windows() {
        let wins = vec![w(0, 0, 100, 100, "wechatappex.exe")];
        assert!(pick_window(&wins, Some((50, 50))).is_none());
    }

    #[test]
    fn live_enumeration_smoke() {
        // 本机没开微信时应返回空 vec 而非崩溃（开着则可能非空）
        let wins = wechat_windows();
        assert!(wins.iter().all(|x| x.w > 0));
    }
}
