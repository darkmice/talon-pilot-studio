//! 主窗口几何持久化:关窗时保存,启动时有记录且仍可见则恢复,否则走屏幕 80% 自适应。
//!
//! 不用 tauri-plugin-window-state:它在窗口创建时自动恢复,与 lib.rs 里「算好尺寸
//! 再 show 防闪跳」的时序耦合;状态文件格式也是插件私有。这里 ~60 行手写,行为可控。
//! 单位统一物理像素(outer_position / inner_size / monitor 原生单位),跨缩放屏不歧义。

use crate::config;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// 状态文件:<app_config_dir>/window-state.json
fn state_file() -> Option<PathBuf> {
    config::app_config_dir().map(|d| d.join("window-state.json"))
}

/// 读上次保存的几何。没存过 / 解析失败(格式演进)都返回 None,走 80% 自适应。
pub fn load() -> Option<WindowGeometry> {
    let content = std::fs::read_to_string(state_file()?).ok()?;
    serde_json::from_str(&content).ok()
}

/// 保存几何。尽力而为:失败只是下次回到 80% 自适应,不值得打扰用户。
pub fn save(g: &WindowGeometry) {
    let Some(file) = state_file() else {
        return;
    };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string(g) {
        let _ = std::fs::write(file, json);
    }
}

/// 记录的几何是否仍"可见":窗口中心点落在任一显示器矩形内(monitors 为
/// `(x, y, width, height)` 物理像素)。显示器拔掉/换分辨率后窗口可能整体在屏外,
/// 此时放弃恢复、走 80% 自适应,避免"窗口找不到了"。
pub fn is_visible_on(g: &WindowGeometry, monitors: &[(i32, i32, u32, u32)]) -> bool {
    let cx = g.x.saturating_add(g.width as i32 / 2);
    let cy = g.y.saturating_add(g.height as i32 / 2);
    monitors.iter().any(|&(mx, my, mw, mh)| {
        cx >= mx
            && cx < mx.saturating_add(mw as i32)
            && cy >= my
            && cy < my.saturating_add(mh as i32)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: (i32, i32, u32, u32) = (0, 0, 2560, 1440);
    const SIDE: (i32, i32, u32, u32) = (2560, 0, 1920, 1080); // 副屏在主屏右侧

    fn geo(x: i32, y: i32, w: u32, h: u32) -> WindowGeometry {
        WindowGeometry {
            x,
            y,
            width: w,
            height: h,
        }
    }

    #[test]
    fn center_on_main_monitor_is_visible() {
        assert!(is_visible_on(&geo(100, 100, 1280, 800), &[MAIN]));
    }

    #[test]
    fn center_on_secondary_monitor_is_visible() {
        // 窗口在副屏:坐标超出主屏但中心在副屏内
        assert!(is_visible_on(&geo(3000, 100, 800, 600), &[MAIN, SIDE]));
    }

    #[test]
    fn window_on_unplugged_monitor_is_not_visible() {
        // 副屏拔掉后,原本在副屏的窗口应判不可见 → 触发 80% 自适应兜底
        assert!(!is_visible_on(&geo(3000, 100, 800, 600), &[MAIN]));
    }

    #[test]
    fn no_monitors_means_not_visible() {
        assert!(!is_visible_on(&geo(0, 0, 100, 100), &[]));
    }

    #[test]
    fn geometry_roundtrips_through_json() {
        let g = geo(-200, 50, 1440, 900); // 负坐标:副屏在主屏左侧的真实场景
        let json = serde_json::to_string(&g).unwrap();
        assert_eq!(serde_json::from_str::<WindowGeometry>(&json).unwrap(), g);
    }
}
